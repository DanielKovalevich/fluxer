// SPDX-License-Identifier: AGPL-3.0-or-later

use super::social::{self, SocialPage};
use super::{ResolveContext, Resolver, ResolverResult};
use crate::direct_media::MediaKind;
use crate::types::{EmbedMedia, MessageEmbed};
use std::future::Future;
use std::pin::Pin;
use url::Url;

pub struct FireshareResolver {
    hosts: Vec<String>,
}

impl FireshareResolver {
    pub fn from_env() -> Self {
        Self {
            hosts: parse_hosts(&std::env::var("FLUXER_UNFURL_FIRESHARE_HOSTS").unwrap_or_default()),
        }
    }
}

impl Resolver for FireshareResolver {
    fn matches(&self, url: &Url) -> bool {
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some_and(|host| {
                self.hosts.iter().any(|allowed| allowed == &host.trim_end_matches('.').to_ascii_lowercase())
            })
            && metadata_url(url).is_some()
    }

    fn resolve<'a>(&'a self, ctx: &'a ResolveContext<'_>) -> Pin<Box<dyn Future<Output = anyhow::Result<ResolverResult>> + Send + 'a>> {
        Box::pin(async move {
            let Some(url) = metadata_url(&ctx.url) else {
                return Ok(ResolverResult { embeds: vec![] });
            };
            let page = social::fetch_page(ctx, &url).await?;
            // Only try variants after public, same-origin metadata advertises a video.
            // Missing/not-yet-generated variants fall back through the normal media
            // proxy checks, including access control, size and content-type limits.
            for video_url in video_candidates(&page, &url) {
                if let Some(video) = social::resolve_media(ctx, &video_url, MediaKind::Video).await {
                    return Ok(ResolverResult {
                        embeds: vec![video_only_embed(&ctx.original_url, video)],
                    });
                }
            }
            Ok(ResolverResult { embeds: vec![] })
        })
    }
}

fn parse_hosts(value: &str) -> Vec<String> {
    value.split(',')
        .map(|host| host.trim().trim_end_matches('.').to_ascii_lowercase())
        .filter(|host| !host.is_empty() && !host.contains(['/', ':', '*', ' ']))
        .collect()
}

fn metadata_url(source: &Url) -> Option<Url> {
    let path = source.path().trim_end_matches('/');
    let mut segments = path.strip_prefix('/')?.split('/');
    let kind = segments.next()?;
    let id = segments.next()?;
    if !matches!(kind, "w" | "watch")
        || id.is_empty()
        || !id.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        || segments.next().is_some()
    {
        return None;
    }
    let mut url = source.clone();
    url.set_path(&format!("/w/{id}"));
    url.set_fragment(None);
    Some(url)
}

fn same_origin_or_https_upgrade(from: &Url, to: &Url) -> bool {
    from.origin() == to.origin()
        || (from.scheme() == "http"
            && to.scheme() == "https"
            && from.host_str() == to.host_str()
            && from.port().is_none()
            && to.port().is_none())
}

fn video_url(page: &SocialPage, metadata_url: &Url) -> Option<String> {
    // Fireshare can advertise HTTPS media on an HTTP watch page. Permit only
    // that same-host upgrade; redirects and media on other sites stay rejected.
    if !same_origin_or_https_upgrade(metadata_url, &page.final_url) {
        return None;
    }
    let value = page.og.video_primary.as_deref()?;
    let resolved = social::resolve_url(&page.final_url, value)?;
    let url = Url::parse(&resolved).ok()?;
    same_origin_or_https_upgrade(&page.final_url, &url).then_some(resolved)
}

fn video_candidates(page: &SocialPage, source: &Url) -> Vec<String> {
    let Some(original) = video_url(page, source) else {
        return vec![];
    };
    let Some(watch) = metadata_url(source) else {
        return vec![original];
    };
    let id = watch.path().trim_start_matches("/w/");
    let mut variant = Url::parse(&original).expect("validated media URL");
    variant.set_path(&format!("/_content/derived/{id}/{id}-720p.mp4"));
    variant.set_query(None);
    vec![variant.to_string(), original]
}

fn video_only_embed(source: &Url, video: EmbedMedia) -> MessageEmbed {
    let mut watch = source.clone();
    let upgraded_media = video
        .url
        .as_deref()
        .and_then(|value| Url::parse(value).ok())
        .is_some_and(|media| {
            source.scheme() == "http"
                && media.scheme() == "https"
                && same_origin_or_https_upgrade(source, &media)
        });
    if upgraded_media {
        watch.set_scheme("https").expect("HTTP URL can upgrade to HTTPS");
    }
    let mut embed = MessageEmbed::new("video");
    embed.url = Some(watch.to_string());
    embed.video = Some(video);
    embed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_only_configured_hosts_and_watch_links() {
        let resolver = FireshareResolver { hosts: parse_hosts(" CLIPS.DKZVER.COM., , other.example ") };
        for path in ["/w/abc_123", "/watch/abc-123/", "/w/abc?start=10"] {
            assert!(resolver.matches(&Url::parse(&format!("https://clips.dkzver.com{path}")).unwrap()));
        }
        for url in [
            "https://evil.clips.dkzver.com/w/abc",
            "https://clips.dkzver.com.evil.example/w/abc",
            "https://clips.dkzver.com/",
            "https://clips.dkzver.com/w/abc/extra",
            "https://clips.dkzver.com/w/abc%2Fdef",
            "https://clips.dkzver.com/watch/",
            "ftp://clips.dkzver.com/w/abc",
        ] {
            assert!(!resolver.matches(&Url::parse(url).unwrap()), "{url}");
        }
        assert!(parse_hosts("").is_empty());
    }

    #[test]
    fn watch_links_use_server_rendered_metadata_and_keep_query() {
        let source = Url::parse("https://clips.dkzver.com/watch/abc?start=12#video").unwrap();
        assert_eq!(metadata_url(&source).unwrap().as_str(), "https://clips.dkzver.com/w/abc?start=12");
    }

    fn page(html: &str) -> SocialPage {
        SocialPage {
            final_url: Url::parse("https://clips.dkzver.com/w/abc").unwrap(),
            content_type: Some("text/html".to_owned()),
            og: crate::html_parser::parse_opengraph(html),
            twitter: Default::default(),
        }
    }

    #[test]
    fn reads_public_video_metadata_but_does_not_guess_missing_media() {
        let metadata = Url::parse("https://clips.dkzver.com/w/abc").unwrap();
        let public = page(r#"<meta property="og:video" content="/_content/video/abc.mp4">"#);
        assert_eq!(video_url(&public, &metadata).as_deref(), Some("https://clips.dkzver.com/_content/video/abc.mp4"));
        let private = page(r#"<meta property="og:title" content="Protected clip">"#);
        assert!(video_url(&private, &metadata).is_none());
        let external = page(r#"<meta property="og:video" content="https://other.example/video.mp4">"#);
        assert!(video_url(&external, &metadata).is_none());
        let mut redirected = public;
        redirected.final_url = Url::parse("https://other.example/w/abc").unwrap();
        assert!(video_url(&redirected, &metadata).is_none());
    }

    #[test]
    fn upgrades_http_watch_links_with_same_host_https_video() {
        let source = Url::parse("http://clips.dkzver.com/w/abc").unwrap();
        let mut public = page(r#"<meta property="og:video" content="https://clips.dkzver.com/_content/video/abc.mp4">"#);
        public.final_url = source.clone();
        assert_eq!(video_candidates(&public, &source), vec![
            "https://clips.dkzver.com/_content/derived/abc/abc-720p.mp4",
            "https://clips.dkzver.com/_content/video/abc.mp4",
        ]);
        let video = EmbedMedia {
            url: Some("https://clips.dkzver.com/_content/derived/abc/abc-720p.mp4".to_owned()),
            ..Default::default()
        };
        assert_eq!(video_only_embed(&source, video).url.as_deref(), Some("https://clips.dkzver.com/w/abc"));

        public.final_url = Url::parse("https://clips.dkzver.com/w/abc").unwrap();
        assert!(video_url(&public, &source).is_some());
    }

    #[test]
    fn rejects_downgrades_other_hosts_and_nonstandard_ports() {
        let https_source = Url::parse("https://clips.dkzver.com/w/abc").unwrap();
        let http_source = Url::parse("http://clips.dkzver.com/w/abc").unwrap();
        let mut public = page(r#"<meta property="og:video" content="http://clips.dkzver.com/_content/video/abc.mp4">"#);
        assert!(video_url(&public, &https_source).is_none());
        public.final_url = http_source.clone();
        public.og = crate::html_parser::parse_opengraph(
            r#"<meta property="og:video" content="https://other.example/video.mp4">"#,
        );
        assert!(video_url(&public, &http_source).is_none());
        public.og = crate::html_parser::parse_opengraph(
            r#"<meta property="og:video" content="https://clips.dkzver.com/_content/video/abc.mp4">"#,
        );
        let custom_port = Url::parse("http://clips.dkzver.com:8080/w/abc").unwrap();
        assert!(video_url(&public, &custom_port).is_none());
    }

    #[test]
    fn prefers_720p_with_original_as_fallback() {
        let source = Url::parse("https://clips.dkzver.com/watch/abc?start=12#video").unwrap();
        let public = page(r#"<meta property="og:video" content="/_content/video/abc.mp4">"#);
        assert_eq!(video_candidates(&public, &source), vec![
            "https://clips.dkzver.com/_content/derived/abc/abc-720p.mp4",
            "https://clips.dkzver.com/_content/video/abc.mp4",
        ]);
    }

    #[test]
    fn does_not_probe_variants_without_public_same_origin_metadata() {
        let source = Url::parse("https://clips.dkzver.com/w/abc").unwrap();
        for html in [
            r#"<meta property="og:title" content="Protected clip">"#,
            r#"<meta property="og:video" content="https://other.example/video.mp4">"#,
        ] {
            assert!(video_candidates(&page(html), &source).is_empty());
        }
        let mut redirected = page(r#"<meta property="og:video" content="/_content/video/abc.mp4">"#);
        redirected.final_url = Url::parse("https://other.example/w/abc").unwrap();
        assert!(video_candidates(&redirected, &source).is_empty());
    }

    #[test]
    fn produces_only_video_without_rich_card_metadata() {
        let source = Url::parse("https://clips.dkzver.com/w/abc").unwrap();
        let video = EmbedMedia { url: Some("https://clips.dkzver.com/_content/video/abc.mp4".to_owned()), ..Default::default() };
        let embed = video_only_embed(&source, video);
        let json = serde_json::to_value(&embed).unwrap();
        assert_eq!(json["type"], "video");
        assert_eq!(json["url"], source.as_str());
        assert_eq!(json.as_object().unwrap().len(), 3);
        assert!(json.get("video").is_some());
    }
}
