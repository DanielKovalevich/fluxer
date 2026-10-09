// SPDX-License-Identifier: AGPL-3.0-or-later

use super::social::{self, SocialPage};
use super::{ResolveContext, Resolver, ResolverResult};
use crate::direct_media::MediaKind;
use crate::oembed;
use crate::text_limits;
use crate::types::{EmbedAuthor, EmbedProvider, MessageEmbed};
use std::future::Future;
use std::pin::Pin;
use url::Url;

const DIRECT_BASE: &str = "https://d.tnktok.com";
const GALLERY_BASE: &str = "https://tnktok.com";

pub struct TikTokResolver {
    enabled: bool,
    direct_base: Url,
    gallery_base: Url,
}

impl TikTokResolver {
    pub fn from_env() -> Self {
        Self {
            enabled: social::read_bool("FLUXER_UNFURL_TIKTOK_ENABLED", true),
            direct_base: env_url("FLUXER_UNFURL_TIKTOK_DIRECT_BASE", DIRECT_BASE),
            gallery_base: env_url("FLUXER_UNFURL_TIKTOK_GALLERY_BASE", GALLERY_BASE),
        }
    }
}

impl Resolver for TikTokResolver {
    fn matches(&self, url: &Url) -> bool {
        self.enabled && is_tiktok_host(url.host_str())
    }

    fn resolve<'a>(&'a self, ctx: &'a ResolveContext<'_>) -> Pin<Box<dyn Future<Output = anyhow::Result<ResolverResult>> + Send + 'a>> {
        Box::pin(async move { resolve_tiktok(ctx, &self.direct_base, &self.gallery_base).await })
    }
}

fn env_url(name: &str, default: &str) -> Url {
    std::env::var(name).ok().and_then(|value| Url::parse(value.trim()).ok()).unwrap_or_else(|| Url::parse(default).expect("valid TikTok fallback URL"))
}

fn is_tiktok_host(host: Option<&str>) -> bool {
    host.is_some_and(|host| {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        host == "tiktok.com" || host.ends_with(".tiktok.com")
    })
}

async fn resolve_tiktok(ctx: &ResolveContext<'_>, direct_base: &Url, gallery_base: &Url) -> anyhow::Result<ResolverResult> {
    let native = social::fetch_page(ctx, &ctx.url).await.ok();
    let source = native.as_ref().map(|page| &page.final_url).unwrap_or(&ctx.url);
    let oembed = match source.host_str().and_then(|host| oembed::known_oembed_endpoint(host, source.as_str())) {
        Some(endpoint) => oembed::fetch_oembed(&ctx.http_client, &endpoint.url, endpoint.format).await.ok(),
        None => None,
    };

    let direct_url = rewrite_to_fixer(direct_base, source);
    let direct_page = social::fetch_page(ctx, &direct_url).await.ok();
    let video_candidate = direct_page.as_ref().and_then(|page| {
        if page.content_type.as_deref().is_some_and(|value| value.starts_with("video/")) {
            Some(direct_url.to_string())
        } else {
            page.og.video_primary.as_deref().or(page.twitter.player.as_deref())
                .and_then(|value| social::resolve_url(&page.final_url, value))
        }
    });
    let video = match video_candidate {
        Some(candidate) => social::resolve_media(ctx, &candidate, MediaKind::Video).await,
        None => None,
    };

    // tnktok's gallery page exposes images for photo posts, while d.tnktok is
    // optimized for playable video. Only request it when video resolution failed.
    let gallery = if video.is_none() { social::fetch_page(ctx, &rewrite_to_fixer(gallery_base, source)).await.ok() } else { None };
    let metadata = direct_page.as_ref().or(native.as_ref()).or(gallery.as_ref());
    let title = oembed.as_ref().and_then(|data| data.title.as_deref())
        .or_else(|| metadata.and_then(|page| page.og.title.as_deref()))
        .map(|value| text_limits::truncate(value.trim(), text_limits::TITLE_MAX));
    let author = oembed.as_ref().and_then(|data| data.author_name.as_deref())
        .or_else(|| metadata.and_then(|page| page.twitter.title.as_deref()))
        .filter(|value| !value.trim().is_empty())
        .map(|value| EmbedAuthor { name: text_limits::truncate(value.trim(), text_limits::AUTHOR_NAME_MAX), ..Default::default() });
    let mut embed = MessageEmbed::new("rich");
    embed.url = Some(ctx.original_url.to_string());
    embed.title = title;
    embed.author = author;
    embed.provider = Some(EmbedProvider { name: "TikTok".to_owned(), url: Some("https://www.tiktok.com".to_owned()) });
    embed.video = video;

    let image_url = metadata.and_then(first_image).or_else(|| gallery.as_ref().and_then(first_image));
    if let Some(image_url) = image_url
        && let Some(image) = social::resolve_media(ctx, &image_url, MediaKind::Image).await
    {
        if embed.video.is_some() { embed.thumbnail = Some(image); } else { embed.image = Some(image); }
    }
    if embed.title.is_none() && embed.author.is_none() && embed.video.is_none() && embed.image.is_none() {
        return Ok(ResolverResult { embeds: vec![] });
    }
    Ok(ResolverResult { embeds: vec![embed] })
}

fn rewrite_to_fixer(base: &Url, source: &Url) -> Url {
    let mut url = base.clone();
    url.set_path(source.path());
    url.set_query(source.query());
    url
}

fn first_image(page: &SocialPage) -> Option<String> {
    page.og.images.first().or(page.og.image.as_ref()).and_then(|value| social::resolve_url(&page.final_url, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_tiktok_short_links() {
        let resolver = TikTokResolver::from_env();
        assert!(resolver.matches(&Url::parse("https://www.tiktok.com/@a/video/1").unwrap()));
        assert!(resolver.matches(&Url::parse("https://vm.tiktok.com/ZM123/").unwrap()));
        assert!(!resolver.matches(&Url::parse("https://example.com/video/1").unwrap()));
    }

    #[test]
    fn rewrites_a_canonical_video_path_to_the_direct_resolver() {
        let direct = Url::parse(DIRECT_BASE).unwrap();
        let source = Url::parse("https://www.tiktok.com/@creator/video/7577840726726249750?lang=en").unwrap();
        assert_eq!(
            rewrite_to_fixer(&direct, &source).as_str(),
            "https://d.tnktok.com/@creator/video/7577840726726249750?lang=en"
        );
    }
}
