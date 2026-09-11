// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reddit embeds backed by the same vxreddit flow used by FixTweetBot.

use super::social::{self, SocialPage};
use super::{ResolveContext, Resolver, ResolverResult};
use crate::direct_media::MediaKind;
use crate::text_limits;
use crate::types::{EmbedAuthor, EmbedMedia, EmbedProvider, MessageEmbed};
use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use url::Url;

const VXREDDIT_BASE: &str = "https://vxreddit.com";
const DISCORD_CRAWLER_UA: &str = "Mozilla/5.0 (compatible; Discordbot/2.0; +https://discordapp.com)";
const REDDIT_COLOR: u32 = 0xFF4500;
const MAX_GALLERY_IMAGES: usize = 10;

pub struct RedditResolver {
    enabled: bool,
    fixer_base: Url,
    user_agent: HeaderValue,
}

impl RedditResolver {
    pub fn from_env() -> Self {
        let fixer_base = std::env::var("FLUXER_UNFURL_REDDIT_FIXER")
            .ok()
            .and_then(|value| Url::parse(value.trim()).ok())
            .unwrap_or_else(|| Url::parse(VXREDDIT_BASE).expect("valid vxreddit URL"));
        let user_agent = std::env::var("FLUXER_UNFURL_REDDIT_USER_AGENT")
            .ok()
            .and_then(|value| HeaderValue::from_str(&value).ok())
            .unwrap_or_else(|| HeaderValue::from_static(DISCORD_CRAWLER_UA));
        Self {
            enabled: social::read_bool("FLUXER_UNFURL_REDDIT_ENABLED", true),
            fixer_base,
            user_agent,
        }
    }
}

impl Resolver for RedditResolver {
    fn matches(&self, url: &Url) -> bool {
        self.enabled && is_reddit_host(url.host_str()) && is_supported_path(url)
    }

    fn resolve<'a>(&'a self, ctx: &'a ResolveContext<'_>) -> Pin<Box<dyn Future<Output = anyhow::Result<ResolverResult>> + Send + 'a>> {
        Box::pin(async move { resolve_reddit(ctx, &self.fixer_base, &self.user_agent).await })
    }
}

fn is_reddit_host(host: Option<&str>) -> bool {
    host.is_some_and(|host| {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        host == "reddit.com" || host.ends_with(".reddit.com") || host == "redditmedia.com" || host.ends_with(".redditmedia.com")
    })
}

// This deliberately follows FixTweetBot's route scope rather than treating
// every Reddit page as a post: posts/comments, short links, and bare post IDs.
fn is_supported_path(url: &Url) -> bool {
    let segments: Vec<_> = url.path_segments().into_iter().flatten().filter(|segment| !segment.is_empty()).collect();
    matches!(segments.as_slice(),
        ["r" | "u" | "user", _, "comments" | "s", _, ..] | [_]
    )
}

async fn resolve_reddit(
    ctx: &ResolveContext<'_>,
    fixer_base: &Url,
    user_agent: &HeaderValue,
) -> anyhow::Result<ResolverResult> {
    let fixer_url = rewrite_to_fixer(fixer_base, &ctx.url);
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, user_agent.clone());
    let Ok(page) = social::fetch_page_with_headers(ctx, &fixer_url, headers).await else {
        return Ok(ResolverResult { embeds: vec![] });
    };

    let video_url = page.og.video_primary.as_deref().or(page.twitter.player.as_deref())
        .and_then(|value| social::resolve_url(&page.final_url, value));
    let video = match video_url {
        Some(url) => social::resolve_media(ctx, &url, MediaKind::Video).await,
        None => None,
    };
    let images = resolve_images(ctx, &page).await;

    let mut embed = MessageEmbed::new("rich");
    embed.url = Some(ctx.original_url.to_string());
    embed.color = Some(REDDIT_COLOR);
    embed.provider = Some(EmbedProvider { name: Some("Reddit".to_owned()), url: Some("https://www.reddit.com".to_owned()) });
    embed.title = page.og.title.as_deref().or(page.twitter.title.as_deref())
        .map(|value| text_limits::truncate(value.trim(), text_limits::TITLE_MAX));
    embed.description = page.og.description.as_deref().or(page.twitter.description.as_deref())
        .map(|value| text_limits::truncate(value.trim(), text_limits::DESCRIPTION_MAX));
    embed.author = reddit_author(&page).map(|name| EmbedAuthor {
        name: text_limits::truncate(&name, text_limits::AUTHOR_NAME_MAX),
        ..Default::default()
    });
    let has_video = video.is_some();
    embed.video = video;
    if has_video {
        embed.thumbnail = images.first().cloned();
    } else {
        embed.image = images.first().cloned();
    }

    if embed.title.is_none() && embed.description.is_none() && embed.author.is_none() && embed.video.is_none() && embed.image.is_none() {
        return Ok(ResolverResult { embeds: vec![] });
    }
    let mut embeds = vec![embed];
    // Fluxer's client turns sibling image embeds with the same URL into an
    // attachment mosaic. Do not add those siblings to video posts: their
    // preview image is a thumbnail, not a gallery item.
    if !has_video {
        append_gallery_images(&mut embeds, &images, &ctx.original_url.to_string());
    }
    Ok(ResolverResult { embeds })
}

fn rewrite_to_fixer(base: &Url, source: &Url) -> Url {
    let mut url = base.clone();
    url.set_path(source.path());
    url.set_query(source.query());
    url
}

async fn resolve_images(ctx: &ResolveContext<'_>, page: &SocialPage) -> Vec<EmbedMedia> {
    let mut images = Vec::new();
    let mut seen_urls = HashSet::new();
    for value in page.og.images.iter().chain(page.og.image.iter()) {
        let Some(url) = social::resolve_url(&page.final_url, value) else {
            continue;
        };
        if !seen_urls.insert(url.to_string()) {
            continue;
        }
        if let Some(image) = social::resolve_media(ctx, &url, MediaKind::Image).await {
            images.push(image);
            if images.len() == MAX_GALLERY_IMAGES {
                break;
            }
        }
    }
    images
}

fn append_gallery_images(embeds: &mut Vec<MessageEmbed>, images: &[EmbedMedia], embed_url: &str) {
    for image in images.iter().skip(1).take(MAX_GALLERY_IMAGES - 1) {
        let mut extra = MessageEmbed::new("rich");
        extra.url = Some(embed_url.to_owned());
        extra.image = Some(image.clone());
        embeds.push(extra);
    }
}

fn reddit_author(page: &SocialPage) -> Option<String> {
    let site_name = page.og.site_name.as_deref()?.trim();
    site_name.split_once(" on r/").map(|(author, _)| author.trim().to_owned()).filter(|author| !author.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_fix_tweet_bot_reddit_routes() {
        let resolver = RedditResolver::from_env();
        assert!(resolver.matches(&Url::parse("https://www.reddit.com/r/test/comments/abc/title/").unwrap()));
        assert!(resolver.matches(&Url::parse("https://reddit.com/u/user/s/abc/").unwrap()));
        assert!(resolver.matches(&Url::parse("https://reddit.com/abc123").unwrap()));
        assert!(!resolver.matches(&Url::parse("https://reddit.com/r/test/about/").unwrap()));
    }

    #[test]
    fn reads_author_from_vxreddit_site_name() {
        let page = SocialPage {
            final_url: Url::parse("https://vxreddit.com/r/test/comments/abc").unwrap(),
            content_type: Some("text/html".to_owned()),
            og: crate::html_parser::OgMetadata { site_name: Some("u/example on r/test - ⬆️ 1".to_owned()), ..Default::default() },
            twitter: Default::default(),
        };
        assert_eq!(reddit_author(&page).as_deref(), Some("u/example"));
    }

    #[test]
    fn appends_images_as_url_matched_gallery_embeds() {
        let first = EmbedMedia { url: Some("https://i.redd.it/one.png".to_owned()), ..Default::default() };
        let second = EmbedMedia { url: Some("https://i.redd.it/two.png".to_owned()), ..Default::default() };
        let third = EmbedMedia { url: Some("https://i.redd.it/three.png".to_owned()), ..Default::default() };
        let mut embeds = vec![MessageEmbed::new("rich")];

        append_gallery_images(&mut embeds, &[first, second, third], "https://www.reddit.com/r/test/comments/abc");

        assert_eq!(embeds.len(), 3);
        assert_eq!(embeds[1].url.as_deref(), Some("https://www.reddit.com/r/test/comments/abc"));
        assert_eq!(embeds[1].image.as_ref().and_then(|image| image.url.as_deref()), Some("https://i.redd.it/two.png"));
        assert_eq!(embeds[2].image.as_ref().and_then(|image| image.url.as_deref()), Some("https://i.redd.it/three.png"));
    }
}
