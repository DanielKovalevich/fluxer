// SPDX-License-Identifier: AGPL-3.0-or-later

use super::social::{self, SocialPage};
use super::{ResolveContext, Resolver, ResolverResult};
use crate::direct_media::MediaKind;
use crate::text_limits;
use crate::types::{EmbedAuthor, EmbedProvider, MessageEmbed};
use std::future::Future;
use std::pin::Pin;
use url::Url;

const INSTAGRAM_FIXERS: &[&str] = &["https://www.uuinstagram.com", "https://www.eeinstagram.com"];
const INSTAGRAM_COLOR: u32 = 0xE1306C;

pub struct InstagramResolver {
    enabled: bool,
    fixers: Vec<Url>,
}

impl InstagramResolver {
    pub fn from_env() -> Self {
        let fixers = std::env::var("FLUXER_UNFURL_INSTAGRAM_FIXERS")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(|value| value.split(',').filter_map(|entry| Url::parse(entry.trim()).ok()).collect())
            .unwrap_or_else(|| INSTAGRAM_FIXERS.iter().filter_map(|entry| Url::parse(entry).ok()).collect());
        Self {
            enabled: social::read_bool("FLUXER_UNFURL_INSTAGRAM_ENABLED", true),
            fixers,
        }
    }
}

impl Resolver for InstagramResolver {
    fn matches(&self, url: &Url) -> bool {
        self.enabled && is_instagram_host(url.host_str()) && is_supported_path(url)
    }

    fn resolve<'a>(&'a self, ctx: &'a ResolveContext<'_>) -> Pin<Box<dyn Future<Output = anyhow::Result<ResolverResult>> + Send + 'a>> {
        Box::pin(async move { resolve_instagram(ctx, &self.fixers).await })
    }
}

fn is_instagram_host(host: Option<&str>) -> bool {
    host.is_some_and(|host| {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        host == "instagram.com" || host.ends_with(".instagram.com")
    })
}

fn is_supported_path(url: &Url) -> bool {
    let segments: Vec<_> = url
        .path_segments()
        .into_iter()
        .flatten()
        .filter(|segment| !segment.is_empty())
        .collect();
    matches!(segments.as_slice(), ["p" | "reel" | "reels", ..] | [_, "p" | "reel" | "reels", ..])
}

async fn resolve_instagram(ctx: &ResolveContext<'_>, fixers: &[Url]) -> anyhow::Result<ResolverResult> {
    let native = social::fetch_page(ctx, &ctx.url).await.ok().filter(|page| !is_post_not_found(page));
    let source = native.as_ref().map(|page| &page.final_url).unwrap_or(&ctx.url);

    let mut fixer_page = None;
    let mut video = None;
    for fixer in fixers {
        let Some(fixer_url) = rewrite_to_fixer(fixer, source) else { continue };
        let Ok(page) = social::fetch_page(ctx, &fixer_url).await else { continue };
        if is_post_not_found(&page) {
            tracing::info!(url = %fixer_url, "Instagram fixer reported post not found; trying next provider");
            continue;
        }
        let candidate = page.og.video_primary.as_deref().or(page.twitter.player.as_deref())
            .and_then(|value| social::resolve_url(&page.final_url, value));
        if let Some(candidate) = candidate
            && let Some(media) = social::resolve_media(ctx, &candidate, MediaKind::Video).await
        {
            video = Some(media);
            fixer_page = Some(page);
            break;
        }
        if fixer_page.is_none() {
            fixer_page = Some(page);
        }
    }

    let metadata = fixer_page.as_ref().or(native.as_ref());
    let Some(metadata) = metadata else { return Ok(ResolverResult { embeds: vec![] }) };
    let author_name = author_name(metadata).or_else(|| native.as_ref().and_then(author_name));
    let description = metadata.og.description.as_deref().or(metadata.twitter.description.as_deref())
        .or_else(|| native.as_ref().and_then(|page| page.og.description.as_deref()))
        .map(clean_description);
    let mut embed = MessageEmbed::new("rich");
    embed.url = Some(ctx.original_url.to_string());
    embed.color = Some(INSTAGRAM_COLOR);
    embed.provider = Some(EmbedProvider { name: Some("Instagram".to_owned()), url: Some("https://www.instagram.com".to_owned()) });
    embed.author = author_name.map(|name| EmbedAuthor { name: text_limits::truncate(&name, text_limits::AUTHOR_NAME_MAX), ..Default::default() });
    embed.description = description.map(|value| text_limits::truncate(&value, text_limits::DESCRIPTION_MAX));
    embed.video = video;

    if embed.video.is_none() {
        let image_url = first_image(metadata).or_else(|| native.as_ref().and_then(first_image));
        if let Some(image_url) = image_url
            && let Some(media) = social::resolve_media(ctx, &image_url, MediaKind::Image).await
        {
            embed.image = Some(media);
        }
    } else if let Some(image_url) = native.as_ref().and_then(first_image)
        && let Some(media) = social::resolve_media(ctx, &image_url, MediaKind::Image).await
    {
        embed.thumbnail = Some(media);
    }

    hide_caption_with_media(&mut embed);

    if embed.author.is_none() && embed.description.is_none() && embed.video.is_none() && embed.image.is_none() {
        return Ok(ResolverResult { embeds: vec![] });
    }
    Ok(ResolverResult { embeds: vec![embed] })
}

fn hide_caption_with_media(embed: &mut MessageEmbed) {
    if embed.video.is_some() || embed.image.is_some() {
        embed.description = None;
    }
}

fn rewrite_to_fixer(base: &Url, source: &Url) -> Option<Url> {
    let mut url = base.clone();
    url.set_path(source.path());
    url.set_query(source.query());
    Some(url)
}

fn first_image(page: &SocialPage) -> Option<String> {
    page.og.images.first().or(page.og.image.as_ref()).and_then(|value| social::resolve_url(&page.final_url, value))
}

fn is_post_not_found(page: &SocialPage) -> bool {
    [
        page.og.title.as_deref(),
        page.og.description.as_deref(),
        page.twitter.title.as_deref(),
        page.twitter.description.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|value| value.trim().eq_ignore_ascii_case("post not found"))
}

fn author_name(page: &SocialPage) -> Option<String> {
    let title = page.twitter.title.as_deref().or(page.og.title.as_deref())?.trim();
    if title.starts_with('@') { return Some(title.to_owned()) }
    title.split_once(" on Instagram").map(|(name, _)| name.trim().to_owned()).filter(|name| !name.is_empty())
}

fn clean_description(value: &str) -> String {
    let value = value.trim();
    let Some((_, caption)) = value.split_once(": ") else { return value.to_owned() };
    if value.chars().next().is_some_and(char::is_numeric) && value.contains(" likes") { caption.to_owned() } else { value.to_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hides_caption_only_when_attached_media_resolves() {
        for kind in ["video", "image"] {
            let mut embed = MessageEmbed::new("rich");
            embed.description = Some("Long caption".to_owned());
            embed.author = Some(EmbedAuthor { name: "@creator".to_owned(), ..Default::default() });
            if kind == "video" {
                embed.video = Some(Default::default());
            } else {
                embed.image = Some(Default::default());
            }
            hide_caption_with_media(&mut embed);
            assert!(embed.description.is_none());
            assert_eq!(embed.author.unwrap().name, "@creator");
        }

        let mut text_only = MessageEmbed::new("rich");
        text_only.description = Some("Fallback caption".to_owned());
        hide_caption_with_media(&mut text_only);
        assert_eq!(text_only.description.as_deref(), Some("Fallback caption"));
    }

    #[test]
    fn matches_posts_and_reels() {
        let resolver = InstagramResolver { enabled: true, fixers: vec![] };
        assert!(resolver.matches(&Url::parse("https://www.instagram.com/reel/abc/").unwrap()));
        assert!(resolver.matches(&Url::parse("https://instagram.com/p/abc/").unwrap()));
        assert!(!resolver.matches(&Url::parse("https://instagram.com/example/").unwrap()));
    }

    #[test]
    fn identifies_post_not_found_placeholder_metadata() {
        let unavailable = SocialPage {
            final_url: Url::parse("https://www.uuinstagram.com/reel/missing/").unwrap(),
            content_type: Some("text/html".to_owned()),
            og: crate::html_parser::OgMetadata {
                description: Some("Post not found".to_owned()),
                ..Default::default()
            },
            twitter: Default::default(),
        };
        let available = SocialPage {
            final_url: Url::parse("https://www.uuinstagram.com/reel/working/").unwrap(),
            content_type: Some("text/html".to_owned()),
            og: crate::html_parser::OgMetadata {
                description: Some("A real caption".to_owned()),
                ..Default::default()
            },
            twitter: Default::default(),
        };

        assert!(is_post_not_found(&unavailable));
        assert!(!is_post_not_found(&available));
    }
}
