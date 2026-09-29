// SPDX-License-Identifier: AGPL-3.0-or-later

//! Shared helpers for the self-hosted social-media resolvers.

use super::ResolveContext;
use crate::charset;
use crate::html_parser::{self, OgMetadata, TwitterCardMetadata};
use crate::http_fetch;
use crate::media_proxy::{embed_media_flags, MediaProxyClient};
use crate::resolvers::media::media_kind_from_content_type;
use crate::types::EmbedMedia;
use crate::direct_media::MediaKind;
use reqwest::header::HeaderMap;
use std::time::Duration;
use url::Url;

pub const SOCIAL_TIMEOUT: Duration = Duration::from_secs(5);
const SOCIAL_HTML_MAX_BYTES: usize = 512 * 1024;

pub struct SocialPage {
    pub final_url: Url,
    pub content_type: Option<String>,
    pub og: OgMetadata,
    pub twitter: TwitterCardMetadata,
}

pub async fn fetch_page(ctx: &ResolveContext<'_>, url: &Url) -> anyhow::Result<SocialPage> {
    fetch_page_with_headers(ctx, url, HeaderMap::new()).await
}

pub async fn fetch_page_with_timeout(
    ctx: &ResolveContext<'_>,
    url: &Url,
    request_timeout: Duration,
) -> anyhow::Result<SocialPage> {
    // Bound the complete fetch, including redirects and the HTML body.
    tokio::time::timeout(
        request_timeout,
        fetch_page_with_headers_and_timeout(ctx, url, HeaderMap::new(), request_timeout),
    )
    .await
    .map_err(|_| anyhow::anyhow!("social page fetch timed out after {} ms", request_timeout.as_millis()))?
}

pub async fn fetch_page_with_headers(
    ctx: &ResolveContext<'_>,
    url: &Url,
    headers: HeaderMap,
) -> anyhow::Result<SocialPage> {
    fetch_page_with_headers_and_timeout(ctx, url, headers, SOCIAL_TIMEOUT).await
}

async fn fetch_page_with_headers_and_timeout(
    ctx: &ResolveContext<'_>,
    url: &Url,
    headers: HeaderMap,
    request_timeout: Duration,
) -> anyhow::Result<SocialPage> {
    // A fixer can answer directly with a video. Read HTML only; downloading a
    // multi-megabyte media body here both wastes bandwidth and prevents us from
    // recognizing its content type as a playable direct-media response.
    let response = http_fetch::fetch_url_with_headers_maybe_body(
        &ctx.http_client,
        url.as_str(),
        headers,
        SOCIAL_HTML_MAX_BYTES,
        request_timeout,
        |head| {
            head.content_type.as_deref().is_none_or(|content_type| {
                content_type.starts_with("text/")
                    || content_type == "application/xhtml+xml"
                    || content_type == "application/xml"
            })
        },
    )
    .await?;
    if response.status != 200 {
        anyhow::bail!("social page returned status {}", response.status);
    }
    let final_url = Url::parse(&response.final_url).unwrap_or_else(|_| url.clone());
    let html = charset::decode_body(
        &response.bytes.unwrap_or_default(),
        response
            .headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
    );
    Ok(SocialPage {
        final_url,
        content_type: response.content_type,
        og: html_parser::parse_opengraph(&html),
        twitter: html_parser::parse_twitter_card(&html),
    })
}

pub fn resolve_url(base: &Url, value: &str) -> Option<String> {
    crate::resolvers::default_helpers::resolve_media_url(base, value)
}

pub async fn resolve_media(
    ctx: &ResolveContext<'_>,
    url: &str,
    expected_kind: MediaKind,
) -> Option<EmbedMedia> {
    match resolve_media_result(ctx, url, expected_kind).await {
        Ok(media) => Some(media),
        Err(err) => {
            tracing::debug!(error = %err, url, "social media metadata lookup failed");
            None
        }
    }
}

pub async fn resolve_media_result(
    ctx: &ResolveContext<'_>,
    url: &str,
    expected_kind: MediaKind,
) -> anyhow::Result<EmbedMedia> {
    let nsfw = MediaProxyClient::nsfw_mode_str(ctx.nsfw_mode);
    let meta = ctx.media_proxy.get_metadata(url, nsfw).await?;
    if media_kind_from_content_type(&meta.content_type) != Some(expected_kind) {
        anyhow::bail!("social media had unexpected content type: {}", meta.content_type);
    }
    Ok(EmbedMedia {
        url: Some(url.to_owned()),
        proxy_url: ctx.media_proxy.external_proxy_url(url),
        content_type: Some(meta.content_type.clone()),
        content_hash: Some(meta.content_hash.clone()),
        width: meta.width,
        height: meta.height,
        placeholder: meta.placeholder.clone(),
        duration: meta.duration.map(|duration| duration as u32),
        flags: embed_media_flags(&meta),
        ..Default::default()
    })
}

pub fn read_bool(name: &str, default: bool) -> bool {
    let Ok(value) = std::env::var(name) else {
        return default;
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => {
            tracing::warn!(variable = name, value, default, "invalid boolean environment value");
            default
        }
    }
}

pub fn read_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}
