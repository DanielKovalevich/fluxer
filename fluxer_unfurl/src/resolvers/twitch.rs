// SPDX-License-Identifier: AGPL-3.0-or-later

use super::social::{self, SOCIAL_TIMEOUT};
use super::{ResolveContext, Resolver, ResolverResult};
use crate::direct_media::MediaKind;
use crate::text_limits;
use crate::types::{EmbedProvider, MessageEmbed};
use serde::Deserialize;
use std::future::Future;
use std::pin::Pin;
use url::Url;

const GQL_ENDPOINT: &str = "https://gql.twitch.tv/gql";
const DEFAULT_CLIENT_ID: &str = "kimne78kx3ncx6brgo4mv6wki5h1ko";
const TWITCH_COLOR: u32 = 0x9146FF;

pub struct TwitchResolver {
    enabled: bool,
    client_id: String,
    max_quality: usize,
}

impl TwitchResolver {
    pub fn from_env() -> Self {
        Self {
            enabled: social::read_bool("FLUXER_UNFURL_TWITCH_CLIPS", true),
            client_id: std::env::var("FLUXER_TWITCH_CLIENT_ID").unwrap_or_else(|_| DEFAULT_CLIENT_ID.to_owned()),
            max_quality: social::read_usize("FLUXER_TWITCH_MAX_QUALITY", 720),
        }
    }
}

impl Resolver for TwitchResolver {
    fn matches(&self, url: &Url) -> bool {
        self.enabled && clip_slug(url).is_some()
    }

    fn resolve<'a>(&'a self, ctx: &'a ResolveContext<'_>) -> Pin<Box<dyn Future<Output = anyhow::Result<ResolverResult>> + Send + 'a>> {
        Box::pin(async move { resolve_twitch(ctx, &self.client_id, self.max_quality).await })
    }
}

async fn resolve_twitch(ctx: &ResolveContext<'_>, client_id: &str, max_quality: usize) -> anyhow::Result<ResolverResult> {
    let Some(slug) = clip_slug(&ctx.url) else { return Ok(ResolverResult { embeds: vec![] }) };
    let (page, media) = tokio::join!(
        social::fetch_page(ctx, &ctx.url),
        resolve_clip_media(ctx, client_id, slug, max_quality),
    );
    let Some(video) = media else { return Ok(ResolverResult { embeds: vec![] }) };
    let page = page.ok();
    let mut embed = MessageEmbed::new("rich");
    embed.url = Some(ctx.original_url.to_string());
    embed.color = Some(TWITCH_COLOR);
    embed.provider = Some(EmbedProvider { name: "Twitch".to_owned(), url: Some("https://www.twitch.tv".to_owned()) });
    embed.title = page.as_ref().and_then(|page| page.og.title.as_deref())
        .map(|value| text_limits::truncate(value.trim(), text_limits::TITLE_MAX));
    embed.description = page.as_ref().and_then(|page| page.og.description.as_deref())
        .map(|value| text_limits::truncate(value.trim(), text_limits::DESCRIPTION_MAX));
    embed.video = Some(video);
    if let Some(image_url) = page.as_ref().and_then(|page| {
        page.og.images.first().or(page.og.image.as_ref())
            .and_then(|value| social::resolve_url(&page.final_url, value))
    })
        && let Some(image) = social::resolve_media(ctx, &image_url, MediaKind::Image).await
    {
        embed.thumbnail = Some(image);
    }
    Ok(ResolverResult { embeds: vec![embed] })
}

async fn resolve_clip_media(ctx: &ResolveContext<'_>, client_id: &str, slug: &str, max_quality: usize) -> Option<crate::types::EmbedMedia> {
    let body = serde_json::json!({
        "operationName": null,
        "variables": { "slug": slug },
        "query": "query($slug: ID!) { clip(slug: $slug) { videoQualities { quality sourceURL } playbackAccessToken(params: {platform: \"web\", playerBackend: \"mediaplayer\", playerType: \"site\"}) { signature value } } }",
    });
    let response = ctx.http_client.post(GQL_ENDPOINT)
        .header("Client-ID", client_id)
        .json(&body)
        .timeout(SOCIAL_TIMEOUT)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() { return None; }
    let payload: GraphQlResponse = response.json().await.ok()?;
    let clip = payload.data?.clip?;
    let token = clip.playback_access_token?;
    let source = choose_quality(&clip.video_qualities, max_quality)?;
    let mut signed = Url::parse(&source.source_url).ok()?;
    signed.query_pairs_mut().append_pair("sig", &token.signature).append_pair("token", &token.value);
    social::resolve_media(ctx, signed.as_str(), MediaKind::Video).await
}

fn clip_slug(url: &Url) -> Option<&str> {
    let host = url.host_str()?.trim_end_matches('.').to_ascii_lowercase();
    let segments: Vec<_> = url.path_segments()?.filter(|segment| !segment.is_empty()).collect();
    if host == "clips.twitch.tv" || host.ends_with(".clips.twitch.tv") {
        return segments.first().copied();
    }
    if host == "twitch.tv" || host.ends_with(".twitch.tv") {
        return segments.windows(2).find_map(|parts| (parts[0] == "clip").then_some(parts[1]));
    }
    None
}

fn choose_quality<'a>(qualities: &'a [VideoQuality], max_quality: usize) -> Option<&'a VideoQuality> {
    let mut candidates: Vec<_> = qualities.iter().filter_map(|quality| quality.quality.parse::<usize>().ok().map(|value| (value, quality))).collect();
    candidates.sort_by_key(|(value, _)| *value);
    candidates.iter().rev().find(|(value, _)| *value <= max_quality).map(|(_, quality)| *quality)
        .or_else(|| candidates.first().map(|(_, quality)| *quality))
}

#[derive(Deserialize)]
struct GraphQlResponse { data: Option<GraphQlData> }
#[derive(Deserialize)]
struct GraphQlData { clip: Option<Clip> }
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Clip { video_qualities: Vec<VideoQuality>, playback_access_token: Option<PlaybackAccessToken> }
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VideoQuality { quality: String, source_url: String }
#[derive(Deserialize)]
struct PlaybackAccessToken { signature: String, value: String }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_twitch_clip_slugs() {
        assert_eq!(clip_slug(&Url::parse("https://clips.twitch.tv/AgileClip-123").unwrap()), Some("AgileClip-123"));
        assert_eq!(clip_slug(&Url::parse("https://www.twitch.tv/name/clip/AgileClip-123").unwrap()), Some("AgileClip-123"));
        assert_eq!(clip_slug(&Url::parse("https://www.twitch.tv/name").unwrap()), None);
    }

    #[test]
    fn chooses_highest_quality_under_cap() {
        let qualities = vec![
            VideoQuality { quality: "1080".to_owned(), source_url: "a".to_owned() },
            VideoQuality { quality: "480".to_owned(), source_url: "b".to_owned() },
            VideoQuality { quality: "720".to_owned(), source_url: "c".to_owned() },
        ];
        assert_eq!(choose_quality(&qualities, 720).map(|quality| quality.source_url.as_str()), Some("c"));
        assert_eq!(choose_quality(&qualities, 360).map(|quality| quality.source_url.as_str()), Some("b"));
    }
}
