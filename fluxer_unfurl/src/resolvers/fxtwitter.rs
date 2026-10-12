// SPDX-License-Identifier: AGPL-3.0-or-later

use super::{ResolveContext, Resolver, ResolverResult};
use crate::http_fetch;
use crate::text_limits;
use crate::types::{EmbedAuthor, EmbedFooter, EmbedMedia, MessageEmbed};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use url::Url;

const FXTWITTER_COLOR: u32 = 0x6364FF;
const FXTWITTER_FOOTER_TEXT: &str = "FxTwitter";
const FXTWITTER_FOOTER_ICON: &str = "https://assets.fxembed.com/logos/fxtwitter64.png";
const FXTWITTER_API_BASE: &str = "https://api.fxtwitter.com";
const API_MAX_BYTES: usize = 512 * 1024;
const API_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_GALLERY_IMAGES: usize = 10;
const MAX_VIDEO_SHORT_EDGE: u32 = 1080;

const FXTWITTER_HOSTS: &[&str] = &["fxtwitter.com", "fixupx.com", "twittpr.com", "xfixup.com"];
const X_HOSTS: &[&str] = &["x.com", "twitter.com"];
const DIRECT_MEDIA_EXTENSIONS: &[&str] = &["mp4", "png", "jpg", "jpeg", "gif", "gifv"];

const EMOJI_REPLY: &str = "\u{1F4AC}";
const EMOJI_RETWEET: &str = "\u{1F501}";
const EMOJI_LIKE: &str = "\u{2764}\u{FE0F}";
const EMOJI_VIEWS: &str = "\u{1F441}\u{FE0F}";
const STAT_SEP: &str = "\u{2002}";
const QUOTE_EMPTY_LINE: &str = "> \u{FE00}";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FxTwitterOptions {
    route_x_statuses: bool,
    show_author: bool,
    show_stats: bool,
    show_footer: bool,
}

impl Default for FxTwitterOptions {
    fn default() -> Self {
        Self {
            route_x_statuses: false,
            show_author: true,
            show_stats: true,
            show_footer: true,
        }
    }
}

impl FxTwitterOptions {
    fn from_env() -> Self {
        Self::from_reader(|name| std::env::var(name).ok())
    }

    fn from_reader<F>(get: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        Self {
            route_x_statuses: read_bool(&get, "FLUXER_UNFURL_X_VIA_FXTWITTER", false),
            show_author: read_bool(&get, "FLUXER_UNFURL_FXTWITTER_SHOW_AUTHOR", true),
            show_stats: read_bool(&get, "FLUXER_UNFURL_FXTWITTER_SHOW_STATS", true),
            show_footer: read_bool(&get, "FLUXER_UNFURL_FXTWITTER_SHOW_FOOTER", true),
        }
    }
}

fn read_bool<F>(get: &F, name: &str, default: bool) -> bool
where
    F: Fn(&str) -> Option<String>,
{
    let Some(value) = get(name) else {
        return default;
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => {
            tracing::warn!(
                variable = name,
                value,
                default,
                "invalid boolean environment value"
            );
            default
        }
    }
}

pub struct FxTwitterResolver {
    options: FxTwitterOptions,
}

impl Default for FxTwitterResolver {
    fn default() -> Self {
        Self {
            options: FxTwitterOptions::default(),
        }
    }
}

impl FxTwitterResolver {
    pub fn from_env() -> Self {
        Self {
            options: FxTwitterOptions::from_env(),
        }
    }
}

impl Resolver for FxTwitterResolver {
    fn matches(&self, url: &Url) -> bool {
        is_fxtwitter_host(url.host_str())
            || (self.options.route_x_statuses
                && is_x_host(url.host_str())
                && parse_status_request(url).is_some())
    }

    fn resolve<'a>(
        &'a self,
        ctx: &'a ResolveContext<'_>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<ResolverResult>> + Send + 'a>> {
        Box::pin(async move {
            let Some(request) = parse_status_request(&ctx.original_url) else {
                return Ok(ResolverResult { embeds: vec![] });
            };
            resolve_status(ctx, &request, self.options).await
        })
    }
}

fn is_fxtwitter_host(host: Option<&str>) -> bool {
    fxtwitter_host_prefixes(host).is_some()
}

fn is_x_host(host: Option<&str>) -> bool {
    let Some(host) = host.map(str::to_ascii_lowercase) else {
        return false;
    };
    X_HOSTS
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

fn fxtwitter_host_prefixes(host: Option<&str>) -> Option<Vec<String>> {
    let host = host?.to_ascii_lowercase();
    let domain = FXTWITTER_HOSTS
        .iter()
        .find(|domain| host == **domain || host.ends_with(&format!(".{}", **domain)))?;
    if host == **domain {
        return Some(Vec::new());
    }
    let prefix = host.strip_suffix(*domain)?.trim_end_matches('.');
    Some(
        prefix
            .split('.')
            .filter(|label| !label.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FxUrlFlags {
    direct: bool,
    text_only: bool,
    gallery: bool,
    force_mosaic: bool,
    force_instant_view: bool,
    old_embed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediaSelectorKind {
    Photo,
    Video,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FxStatusRequest {
    screen_name: String,
    status_id: String,
    language: Option<String>,
    media_index: Option<usize>,
    media_kind: Option<MediaSelectorKind>,
    direct_media_name: Option<String>,
    flags: FxUrlFlags,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedStatusId {
    id: String,
    direct_media_suffix: bool,
    direct_media_name: Option<String>,
}

fn parse_status_request(url: &Url) -> Option<FxStatusRequest> {
    let mut flags = flags_from_host(url);
    let mut segments: Vec<&str> = url
        .path()
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();

    if matches!(segments.first().copied(), Some("dir" | "dl")) {
        flags.direct = true;
        segments.remove(0);
    }

    let status_idx = segments
        .iter()
        .position(|segment| *segment == "status" || *segment == "statuses")?;
    let parsed_id = parse_status_id_segment(segments.get(status_idx + 1)?)?;
    if parsed_id.direct_media_suffix {
        flags.direct = true;
    }

    let screen_name = if status_idx == 0 {
        "i"
    } else {
        segments.first().copied().unwrap_or("i")
    };
    let mut suffix = &segments[(status_idx + 2)..];
    let mut media_kind = None;
    let mut media_index = None;
    if suffix.len() >= 2
        && let Some(kind) = parse_media_selector_kind(suffix[0])
        && let Some(index) = parse_media_selector_index(suffix[1])
    {
        media_kind = Some(kind);
        media_index = Some(index);
        suffix = &suffix[2..];
    }
    let language = suffix
        .first()
        .filter(|segment| is_language_suffix(segment))
        .map(|segment| (*segment).to_owned());

    let query_name = url
        .query_pairs()
        .find(|(key, _)| key == "name")
        .map(|(_, value)| value.into_owned());

    Some(FxStatusRequest {
        screen_name: screen_name.to_owned(),
        status_id: parsed_id.id,
        language,
        media_index,
        media_kind,
        direct_media_name: query_name.or(parsed_id.direct_media_name),
        flags,
    })
}

fn flags_from_host(url: &Url) -> FxUrlFlags {
    let mut flags = FxUrlFlags::default();
    let Some(prefixes) = fxtwitter_host_prefixes(url.host_str()) else {
        return flags;
    };
    for prefix in prefixes {
        match prefix.as_str() {
            "d" | "dl" => flags.direct = true,
            "t" => flags.text_only = true,
            "g" => flags.gallery = true,
            "m" => flags.force_mosaic = true,
            "i" => flags.force_instant_view = true,
            "o" => flags.old_embed = true,
            "www" => {}
            _ => {}
        }
    }
    flags
}

fn parse_status_id_segment(segment: &str) -> Option<ParsedStatusId> {
    let digit_end = segment
        .bytes()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if digit_end == 0 {
        return None;
    }

    let id = segment[..digit_end].to_owned();
    let rest = &segment[digit_end..];
    if rest.is_empty() {
        return Some(ParsedStatusId {
            id,
            direct_media_suffix: false,
            direct_media_name: None,
        });
    }

    if let Some(name) = rest.strip_prefix(':') {
        return Some(ParsedStatusId {
            id,
            direct_media_suffix: false,
            direct_media_name: (!name.is_empty()).then(|| name.to_owned()),
        });
    }

    let ext_and_name = rest.strip_prefix('.')?;
    let (extension, name) = ext_and_name
        .split_once(':')
        .map_or((ext_and_name, None), |(extension, name)| {
            (extension, (!name.is_empty()).then_some(name))
        });
    if !DIRECT_MEDIA_EXTENSIONS
        .iter()
        .any(|known| extension.eq_ignore_ascii_case(known))
    {
        return None;
    }

    Some(ParsedStatusId {
        id,
        direct_media_suffix: true,
        direct_media_name: name.map(str::to_owned),
    })
}

fn parse_media_selector_kind(segment: &str) -> Option<MediaSelectorKind> {
    match segment {
        "photo" | "photos" => Some(MediaSelectorKind::Photo),
        "video" | "videos" => Some(MediaSelectorKind::Video),
        _ => None,
    }
}

fn parse_media_selector_index(segment: &str) -> Option<usize> {
    let index = segment.parse::<usize>().ok()?;
    (1..=4).contains(&index).then_some(index - 1)
}

fn is_language_suffix(segment: &str) -> bool {
    let mut parts = segment.split('-');
    let Some(language) = parts.next() else {
        return false;
    };
    if !(2..=3).contains(&language.len())
        || !language.bytes().all(|byte| byte.is_ascii_alphabetic())
    {
        return false;
    }
    parts.all(|part| {
        (2..=8).contains(&part.len())
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    })
}

#[derive(Debug, Deserialize)]
struct FxResponse {
    tweet: Option<FxTweet>,
}

#[derive(Debug, Deserialize)]
struct FxTweet {
    id: Option<String>,
    text: Option<String>,
    created_timestamp: Option<i64>,
    author: Option<FxAuthor>,
    replies: Option<u64>,
    #[serde(alias = "reposts")]
    retweets: Option<u64>,
    likes: Option<u64>,
    views: Option<u64>,
    quote: Option<Box<FxQuote>>,
    media: Option<FxMedia>,
    translation: Option<FxTranslation>,
}

#[derive(Debug, Deserialize)]
struct FxAuthor {
    screen_name: Option<String>,
    name: Option<String>,
    avatar_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FxQuote {
    url: Option<String>,
    text: Option<String>,
    author: Option<FxAuthor>,
    media: Option<FxMedia>,
}

#[derive(Debug, Deserialize)]
struct FxTranslation {
    text: Option<String>,
    source_lang: Option<String>,
    target_lang: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct FxMedia {
    all: Option<Vec<FxMediaItem>>,
    photos: Option<Vec<FxPhoto>>,
    videos: Option<Vec<FxVideo>>,
    mosaic: Option<FxMosaicPhoto>,
}

impl FxMedia {
    fn has_any(&self) -> bool {
        self.all.as_ref().is_some_and(|all| !all.is_empty())
            || self.photos.as_ref().is_some_and(|p| !p.is_empty())
            || self.videos.as_ref().is_some_and(|v| !v.is_empty())
            || self
                .mosaic
                .as_ref()
                .and_then(FxMosaicPhoto::image_url)
                .is_some()
    }
}

#[derive(Debug, Deserialize)]
struct FxMediaItem {
    #[serde(rename = "type")]
    item_type: Option<String>,
    url: Option<String>,
    thumbnail_url: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    duration: Option<f64>,
    transcode_url: Option<String>,
    #[serde(default, deserialize_with = "deserialize_media_formats")]
    formats: FxMediaFormats,
    #[serde(default)]
    variants: Vec<FxVideoVariant>,
}

#[derive(Debug, Deserialize)]
struct FxPhoto {
    url: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct FxMosaicPhoto {
    url: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    formats: Option<FxMosaicFormats>,
}

#[derive(Debug, Deserialize)]
struct FxMosaicFormats {
    jpeg: Option<String>,
    webp: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FxVideoFormat {
    url: Option<String>,
    bitrate: Option<u64>,
    container: Option<String>,
    codec: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FxVideoVariant {
    url: Option<String>,
    bitrate: Option<u64>,
    content_type: Option<String>,
}

#[derive(Debug, Default)]
struct FxMediaFormats {
    image: Option<FxMosaicFormats>,
    videos: Vec<FxVideoFormat>,
}

impl FxMediaFormats {
    fn image_url(&self) -> Option<&str> {
        self.image
            .as_ref()
            .and_then(|formats| formats.jpeg.as_deref().or(formats.webp.as_deref()))
    }
}

fn deserialize_media_formats<'de, D>(deserializer: D) -> Result<FxMediaFormats, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<Value>::deserialize(deserializer)? {
        Some(Value::Object(map)) => serde_json::from_value(Value::Object(map))
            .map(|image| FxMediaFormats {
                image: Some(image),
                videos: Vec::new(),
            })
            .map_err(serde::de::Error::custom),
        Some(Value::Array(items)) => serde_json::from_value(Value::Array(items))
            .map(|videos| FxMediaFormats {
                image: None,
                videos,
            })
            .map_err(serde::de::Error::custom),
        _ => Ok(FxMediaFormats::default()),
    }
}

impl FxMosaicPhoto {
    fn image_url(&self) -> Option<&str> {
        self.formats
            .as_ref()
            .and_then(|formats| formats.jpeg.as_deref().or(formats.webp.as_deref()))
            .or(self.url.as_deref())
    }
}

#[derive(Debug, Deserialize)]
struct FxVideo {
    url: Option<String>,
    thumbnail_url: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    duration: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_media_formats")]
    formats: FxMediaFormats,
    #[serde(default)]
    variants: Vec<FxVideoVariant>,
}

#[derive(Debug, Clone, Copy)]
enum MediaChoice<'a> {
    Image {
        url: &'a str,
        width: Option<u32>,
        height: Option<u32>,
    },
    Video {
        url: &'a str,
        thumbnail_url: Option<&'a str>,
        width: Option<u32>,
        height: Option<u32>,
        duration: Option<f64>,
    },
}

async fn resolve_status(
    ctx: &ResolveContext<'_>,
    request: &FxStatusRequest,
    options: FxTwitterOptions,
) -> anyhow::Result<ResolverResult> {
    let api_url = fxtwitter_api_url(request);
    let result =
        http_fetch::fetch_url(&ctx.http_client, &api_url, API_MAX_BYTES, API_TIMEOUT).await?;
    if result.status != 200 {
        return Ok(ResolverResult { embeds: vec![] });
    }
    let Ok(response) = serde_json::from_slice::<FxResponse>(&result.bytes) else {
        return Ok(ResolverResult { embeds: vec![] });
    };
    let Some(tweet) = response.tweet else {
        return Ok(ResolverResult { embeds: vec![] });
    };

    if request.flags.direct
        && let Some(embed) = build_direct_media_embed(ctx, &tweet, request).await
    {
        return Ok(ResolverResult {
            embeds: vec![embed],
        });
    }

    let posted_url = ctx.original_url.to_string();
    let mut embed = MessageEmbed::new("rich");
    embed.url = Some(posted_url.clone());
    if !request.flags.gallery {
        embed.color = Some(FXTWITTER_COLOR);
        embed.description = Some(build_description(&tweet, options.show_stats));
        embed.timestamp = tweet.created_timestamp.and_then(format_timestamp);
        if options.show_footer {
            embed.footer = Some(build_fxtwitter_footer(ctx));
        }
    }

    if options.show_author
        && let Some(author) = tweet.author.as_ref()
    {
        embed.author = Some(build_fxtwitter_author(
            ctx,
            author,
            &posted_url,
            &request.screen_name,
        ));
    }

    let mut gallery: Vec<EmbedMedia> = Vec::new();
    if !request.flags.text_only {
        apply_media_to_embed(ctx, &tweet, request, &mut embed, &mut gallery).await;
    }

    let mut embeds = vec![embed];
    for image in gallery {
        let mut extra = MessageEmbed::new("rich");
        extra.url = Some(posted_url.clone());
        extra.image = Some(image);
        embeds.push(extra);
    }

    Ok(ResolverResult { embeds })
}

fn build_fxtwitter_footer(ctx: &ResolveContext<'_>) -> EmbedFooter {
    EmbedFooter {
        text: FXTWITTER_FOOTER_TEXT.to_owned(),
        icon_url: Some(FXTWITTER_FOOTER_ICON.to_owned()),
        proxy_icon_url: ctx.media_proxy.external_proxy_url(FXTWITTER_FOOTER_ICON),
    }
}

fn build_fxtwitter_author(
    ctx: &ResolveContext<'_>,
    author: &FxAuthor,
    posted_url: &str,
    fallback_screen_name: &str,
) -> EmbedAuthor {
    let screen = author
        .screen_name
        .as_deref()
        .unwrap_or(fallback_screen_name);
    let name = author.name.as_deref().unwrap_or(screen);
    EmbedAuthor {
        name: text_limits::truncate(&format!("{name} (@{screen})"), text_limits::AUTHOR_NAME_MAX),
        url: Some(posted_url.to_owned()),
        icon_url: author.avatar_url.clone(),
        proxy_icon_url: author
            .avatar_url
            .as_deref()
            .and_then(|url| ctx.media_proxy.external_proxy_url(url)),
    }
}

fn fxtwitter_api_url(request: &FxStatusRequest) -> String {
    let mut api_url = format!(
        "{FXTWITTER_API_BASE}/{}/status/{}",
        request.screen_name, request.status_id
    );
    if let Some(language) = request.language.as_deref() {
        api_url.push('/');
        api_url.push_str(language);
    }
    api_url
}

async fn build_direct_media_embed(
    ctx: &ResolveContext<'_>,
    tweet: &FxTweet,
    request: &FxStatusRequest,
) -> Option<MessageEmbed> {
    let media = tweet.media.as_ref()?;
    let choice = if let Some(index) = request.media_index {
        selected_choice_from_media(media, index, request.media_kind)
    } else if request.flags.force_mosaic {
        mosaic_choice(media).or_else(|| first_direct_choice_from_media(media))
    } else {
        first_direct_choice_from_media(media)
    }?;

    build_direct_media_embed_for_choice(ctx, choice, request.direct_media_name.as_deref()).await
}

async fn build_direct_media_embed_for_choice(
    ctx: &ResolveContext<'_>,
    choice: MediaChoice<'_>,
    image_name: Option<&str>,
) -> Option<MessageEmbed> {
    match choice {
        MediaChoice::Image { url, width, height } => {
            let media_url = apply_image_name(url, image_name);
            let media = build_image_media(ctx, &media_url, width, height).await?;
            let mut embed = MessageEmbed::new("image");
            embed.url = Some(media_url);
            embed.thumbnail = Some(media);
            Some(embed)
        }
        MediaChoice::Video {
            url,
            thumbnail_url,
            width,
            height,
            duration,
        } => {
            let (thumbnail, video) =
                build_video_pair_from_parts(ctx, url, thumbnail_url, width, height, duration).await;
            let video = video?;
            let mut embed = MessageEmbed::new("video");
            embed.url = video.url.clone();
            embed.thumbnail = thumbnail;
            embed.video = Some(video);
            Some(embed)
        }
    }
}

async fn apply_media_to_embed(
    ctx: &ResolveContext<'_>,
    tweet: &FxTweet,
    request: &FxStatusRequest,
    embed: &mut MessageEmbed,
    gallery: &mut Vec<EmbedMedia>,
) {
    let Some(media) = select_media_source(tweet) else {
        return;
    };

    if let Some(index) = request.media_index {
        if let Some(choice) = selected_choice_from_media(media, index, request.media_kind) {
            apply_media_choice(ctx, embed, choice).await;
        }
        return;
    }

    if request.flags.force_mosaic
        && let Some(choice) = mosaic_choice(media)
        && apply_media_choice(ctx, embed, choice).await
    {
        return;
    }

    if let Some(video) = media.videos.as_ref().and_then(|videos| videos.first()) {
        let (thumbnail, video_media) = build_video_pair(ctx, video).await;
        embed.thumbnail = thumbnail;
        embed.video = video_media;
    } else if let Some(photos) = media.photos.as_ref() {
        let mut photos = photos.iter();
        if let Some(first) = photos.next()
            && let Some(image) = build_photo_media(ctx, first).await
        {
            embed.image = Some(image);
        }
        for photo in photos.take(MAX_GALLERY_IMAGES.saturating_sub(1)) {
            if let Some(image) = build_photo_media(ctx, photo).await {
                gallery.push(image);
            }
        }
    } else if let Some(choice) = first_choice_from_all(media) {
        apply_media_choice(ctx, embed, choice).await;
    }
}

async fn apply_media_choice(
    ctx: &ResolveContext<'_>,
    embed: &mut MessageEmbed,
    choice: MediaChoice<'_>,
) -> bool {
    match choice {
        MediaChoice::Image { url, width, height } => {
            let Some(image) = build_image_media(ctx, url, width, height).await else {
                return false;
            };
            embed.image = Some(image);
            true
        }
        MediaChoice::Video {
            url,
            thumbnail_url,
            width,
            height,
            duration,
        } => {
            let (thumbnail, video_media) =
                build_video_pair_from_parts(ctx, url, thumbnail_url, width, height, duration).await;
            embed.thumbnail = thumbnail;
            embed.video = video_media;
            embed.video.is_some()
        }
    }
}

fn select_media_source(tweet: &FxTweet) -> Option<&FxMedia> {
    if let Some(media) = tweet.media.as_ref().filter(|media| media.has_any()) {
        return Some(media);
    }
    tweet
        .quote
        .as_deref()
        .and_then(|quote| quote.media.as_ref())
        .filter(|media| media.has_any())
}

fn selected_choice_from_media<'a>(
    media: &'a FxMedia,
    index: usize,
    media_kind: Option<MediaSelectorKind>,
) -> Option<MediaChoice<'a>> {
    media
        .all
        .as_ref()
        .and_then(|all| all.get(index))
        .and_then(media_item_choice)
        .or_else(|| match media_kind {
            Some(MediaSelectorKind::Photo) => media
                .photos
                .as_ref()
                .and_then(|photos| photos.get(index))
                .and_then(photo_choice),
            Some(MediaSelectorKind::Video) => media
                .videos
                .as_ref()
                .and_then(|videos| videos.get(index))
                .and_then(video_choice),
            None => first_direct_choice_from_media(media),
        })
}

fn first_direct_choice_from_media(media: &FxMedia) -> Option<MediaChoice<'_>> {
    first_choice_from_all(media)
        .or_else(|| media.photos.as_ref()?.first().and_then(photo_choice))
        .or_else(|| media.videos.as_ref()?.first().and_then(video_choice))
        .or_else(|| mosaic_choice(media))
}

fn first_choice_from_all(media: &FxMedia) -> Option<MediaChoice<'_>> {
    media
        .all
        .as_ref()
        .and_then(|all| all.first())
        .and_then(media_item_choice)
}

fn mosaic_choice(media: &FxMedia) -> Option<MediaChoice<'_>> {
    let mosaic = media.mosaic.as_ref()?;
    Some(MediaChoice::Image {
        url: mosaic.image_url()?,
        width: mosaic.width,
        height: mosaic.height,
    })
}

fn photo_choice(photo: &FxPhoto) -> Option<MediaChoice<'_>> {
    Some(MediaChoice::Image {
        url: photo.url.as_deref()?,
        width: photo.width,
        height: photo.height,
    })
}

fn video_choice(video: &FxVideo) -> Option<MediaChoice<'_>> {
    let selected = preferred_video(
        video.url.as_deref(),
        video.width,
        video.height,
        &video.formats.videos,
        &video.variants,
    )?;
    Some(MediaChoice::Video {
        url: selected.url,
        thumbnail_url: video.thumbnail_url.as_deref(),
        width: selected.width,
        height: selected.height,
        duration: video.duration,
    })
}

fn media_item_choice(item: &FxMediaItem) -> Option<MediaChoice<'_>> {
    let item_type = item.item_type.as_deref().unwrap_or_default();
    let is_video = item_type == "video"
        || (item_type == "gif" && (item.thumbnail_url.is_some() || item.duration.is_some()))
        || item.thumbnail_url.is_some()
        || item.duration.is_some();

    if is_video {
        let selected = preferred_video(
            item.transcode_url.as_deref().or(item.url.as_deref()),
            item.width,
            item.height,
            &item.formats.videos,
            &item.variants,
        )?;
        return Some(MediaChoice::Video {
            url: selected.url,
            thumbnail_url: item.thumbnail_url.as_deref(),
            width: selected.width,
            height: selected.height,
            duration: item.duration,
        });
    }

    let image_url = item
        .formats
        .image_url()
        .or(item.url.as_deref())?;
    Some(MediaChoice::Image {
        url: image_url,
        width: item.width,
        height: item.height,
    })
}

#[derive(Debug, Clone, Copy)]
struct SelectedVideo<'a> {
    url: &'a str,
    width: Option<u32>,
    height: Option<u32>,
}

fn preferred_video<'a>(
    original_url: Option<&'a str>,
    original_width: Option<u32>,
    original_height: Option<u32>,
    formats: &'a [FxVideoFormat],
    variants: &'a [FxVideoVariant],
) -> Option<SelectedVideo<'a>> {
    let format_candidates = formats
        .iter()
        .filter(|format| {
            format
                .container
                .as_deref()
                .is_none_or(|container| container.eq_ignore_ascii_case("mp4"))
                && format
                    .codec
                    .as_deref()
                    .is_none_or(|codec| codec.eq_ignore_ascii_case("h264"))
        })
        .filter_map(|format| {
            let url = format.url.as_deref()?;
            let (width, height) = video_dimensions_from_url(url)?;
            (width.min(height) <= MAX_VIDEO_SHORT_EDGE).then_some((
                format.bitrate.unwrap_or_default(),
                u64::from(width) * u64::from(height),
                url,
                width,
                height,
            ))
        });
    let variant_candidates = variants.iter().filter_map(|variant| {
        let url = variant.url.as_deref()?;
        if !variant
            .content_type
            .as_deref()
            .is_some_and(|content_type| content_type.eq_ignore_ascii_case("video/mp4"))
        {
            return None;
        }
        let (width, height) = video_dimensions_from_url(url)?;
        (width.min(height) <= MAX_VIDEO_SHORT_EDGE).then_some((
            variant.bitrate.unwrap_or_default(),
            u64::from(width) * u64::from(height),
            url,
            width,
            height,
        ))
    });
    let selected = format_candidates
        .chain(variant_candidates)
        .max_by_key(|(bitrate, pixels, ..)| (*bitrate, *pixels));

    if let Some((_, _, url, width, height)) = selected {
        return Some(SelectedVideo {
            url,
            width: Some(width),
            height: Some(height),
        });
    }

    Some(SelectedVideo {
        url: original_url?,
        width: original_width,
        height: original_height,
    })
}

fn video_dimensions_from_url(url: &str) -> Option<(u32, u32)> {
    Url::parse(url).ok()?.path_segments()?.find_map(|segment| {
        let (width, height) = segment.split_once('x')?;
        Some((width.parse().ok()?, height.parse().ok()?))
    })
}

async fn build_photo_media(ctx: &ResolveContext<'_>, photo: &FxPhoto) -> Option<EmbedMedia> {
    let url = photo.url.as_deref()?;
    build_image_media(ctx, url, photo.width, photo.height).await
}

async fn build_image_media(
    ctx: &ResolveContext<'_>,
    url: &str,
    width: Option<u32>,
    height: Option<u32>,
) -> Option<EmbedMedia> {
    let nsfw = crate::media_proxy::MediaProxyClient::nsfw_mode_str(ctx.nsfw_mode);
    let meta = ctx.media_proxy.get_metadata(url, nsfw).await.ok()?;
    Some(EmbedMedia {
        proxy_url: ctx.media_proxy.external_proxy_url(url),
        width: width.or(meta.width),
        height: height.or(meta.height),
        ..meta.to_embed_media(url)
    })
}

async fn build_video_pair(
    ctx: &ResolveContext<'_>,
    video: &FxVideo,
) -> (Option<EmbedMedia>, Option<EmbedMedia>) {
    let Some(selected) = preferred_video(
        video.url.as_deref(),
        video.width,
        video.height,
        &video.formats.videos,
        &video.variants,
    ) else {
        return (None, None);
    };
    build_video_pair_from_parts(
        ctx,
        selected.url,
        video.thumbnail_url.as_deref(),
        selected.width,
        selected.height,
        video.duration,
    )
    .await
}

async fn build_video_pair_from_parts(
    ctx: &ResolveContext<'_>,
    video_url: &str,
    thumbnail_url: Option<&str>,
    width: Option<u32>,
    height: Option<u32>,
    duration: Option<f64>,
) -> (Option<EmbedMedia>, Option<EmbedMedia>) {
    let nsfw = crate::media_proxy::MediaProxyClient::nsfw_mode_str(ctx.nsfw_mode);

    let thumbnail = match thumbnail_url {
        Some(thumb_url) => match ctx.media_proxy.get_metadata(thumb_url, nsfw).await {
            Ok(meta) => Some(EmbedMedia {
                proxy_url: ctx.media_proxy.external_proxy_url(thumb_url),
                duration: None,
                ..meta.to_embed_media(thumb_url)
            }),
            Err(_) => None,
        },
        None => None,
    };

    if video_url.is_empty() {
        return (thumbnail, None);
    }
    let Ok(meta) = ctx.media_proxy.get_metadata(video_url, nsfw).await else {
        return (thumbnail, None);
    };
    let video_media = EmbedMedia {
        proxy_url: ctx.media_proxy.external_proxy_url(video_url),
        width: width.or(meta.width),
        height: height.or(meta.height),
        duration: duration.or(meta.duration).map(|duration| duration as u32),
        ..meta.to_embed_media(video_url)
    };
    (thumbnail, Some(video_media))
}

fn apply_image_name(url: &str, name: Option<&str>) -> String {
    let Some(name) = name else {
        return url.to_owned();
    };
    let Ok(mut parsed) = Url::parse(url) else {
        return url.to_owned();
    };

    let path = parsed.path().to_owned();
    if let Some(colon_idx) = path.rfind(':') {
        let slash_idx = path.rfind('/').unwrap_or(0);
        if colon_idx > slash_idx {
            parsed.set_path(&path[..colon_idx]);
        }
    }
    let query_pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(key, _)| key != "name")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    {
        let mut query = parsed.query_pairs_mut();
        query.clear();
        for (key, value) in query_pairs {
            query.append_pair(&key, &value);
        }
        if !name.is_empty() {
            query.append_pair("name", name);
        }
    }
    parsed.to_string()
}

fn build_description(tweet: &FxTweet, show_stats: bool) -> String {
    let mut sections: Vec<String> = Vec::new();
    let text = tweet.text.as_deref().unwrap_or("");
    if !text.is_empty() {
        sections.push(format_body(text));
    }
    if let Some(translation) = tweet.translation.as_ref().and_then(build_translation_block) {
        sections.push(translation);
    }
    if let Some(quote) = tweet.quote.as_deref() {
        sections.push(build_quote_block(quote));
    }
    if show_stats {
        sections.push(build_stats(tweet));
    }
    sections.join("\n\n")
}

fn build_translation_block(translation: &FxTranslation) -> Option<String> {
    let text = translation
        .text
        .as_deref()
        .filter(|text| !text.is_empty())?;
    let label = match (
        translation.source_lang.as_deref(),
        translation.target_lang.as_deref(),
    ) {
        (Some(source), Some(target)) => format!("**Translation ({source} -> {target})**"),
        (_, Some(target)) => format!("**Translation ({target})**"),
        _ => "**Translation**".to_owned(),
    };
    Some(format!("{label}\n{}", format_body(text)))
}

fn build_quote_block(quote: &FxQuote) -> String {
    let screen = quote
        .author
        .as_ref()
        .and_then(|author| author.screen_name.as_deref())
        .unwrap_or_default();
    let name = quote
        .author
        .as_ref()
        .and_then(|author| author.name.as_deref())
        .unwrap_or(screen);
    let quote_url = quote
        .url
        .clone()
        .unwrap_or_else(|| format!("https://x.com/{screen}/status/"));
    let author_url = format!("https://x.com/{screen}");
    let header = format!(
        "> **[Quoting]({quote_url}) {name} \\([@{screen}]({author_url})\\)**",
        name = escape_markdown(name),
    );
    let body = format_body(quote.text.as_deref().unwrap_or(""))
        .split('\n')
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{header}\n{QUOTE_EMPTY_LINE}\n{body}")
}

fn build_stats(tweet: &FxTweet) -> String {
    let id = tweet.id.as_deref().unwrap_or_default();
    let mut out = String::from("**");
    out.push_str(&stat_segment(
        EMOJI_REPLY,
        &format!("https://x.com/intent/tweet?in_reply_to={id}"),
        tweet.replies.unwrap_or(0),
    ));
    out.push_str(&stat_segment(
        EMOJI_RETWEET,
        &format!("https://x.com/intent/retweet?tweet_id={id}"),
        tweet.retweets.unwrap_or(0),
    ));
    out.push_str(&stat_segment(
        EMOJI_LIKE,
        &format!("https://x.com/intent/like?tweet_id={id}"),
        tweet.likes.unwrap_or(0),
    ));
    if let Some(views) = tweet.views {
        out.push_str(EMOJI_VIEWS);
        out.push(' ');
        out.push_str(&escape_markdown(&format_number(views)));
        out.push_str(STAT_SEP);
    }
    out.push_str("**");
    out
}

fn stat_segment(emoji: &str, intent_url: &str, count: u64) -> String {
    format!(
        "[{emoji}]({intent_url}) {count}{STAT_SEP}",
        count = escape_markdown(&format_number(count)),
    )
}

fn format_number(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.2}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}K", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

fn format_body(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((start, end)) = next_url(rest) {
        out.push_str(&escape_markdown(&rest[..start]));
        out.push_str(&linkify_url(&rest[start..end]));
        rest = &rest[end..];
    }
    out.push_str(&escape_markdown(rest));
    out
}

fn next_url(text: &str) -> Option<(usize, usize)> {
    const SCHEMES: [&str; 2] = ["https://", "http://"];
    for (start, _) in text.char_indices() {
        let rest = &text[start..];
        if SCHEMES.iter().any(|scheme| rest.starts_with(scheme)) {
            let end = rest
                .find(char::is_whitespace)
                .map_or(text.len(), |offset| start + offset);
            return Some((start, end));
        }
    }
    None
}

fn linkify_url(url: &str) -> String {
    let label = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    format!("[{label}]({url})")
}

fn escape_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(
            ch,
            '\\' | '*' | '_' | '~' | '`' | '|' | '>' | '[' | ']' | '(' | ')' | '.'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn format_timestamp(unix_seconds: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(unix_seconds, 0)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn assert_video_choice(
        choice: MediaChoice<'_>,
        expected_url: &str,
        expected_thumbnail_url: Option<&str>,
        expected_width: Option<u32>,
        expected_height: Option<u32>,
        expected_duration: Option<f64>,
    ) {
        match choice {
            MediaChoice::Video {
                url,
                thumbnail_url,
                width,
                height,
                duration,
            } => {
                assert_eq!(url, expected_url);
                assert_eq!(thumbnail_url, expected_thumbnail_url);
                assert_eq!(width, expected_width);
                assert_eq!(height, expected_height);
                assert_eq!(duration, expected_duration);
            }
            MediaChoice::Image { .. } => panic!("expected video media choice"),
        }
    }

    fn quote_example() -> FxQuote {
        FxQuote {
            url: Some("https://x.com/ScandinavianAE/status/2050646038420045877".to_owned()),
            text: Some("Sweden \u{1F1F8}\u{1F1EA}".to_owned()),
            author: Some(FxAuthor {
                screen_name: Some("ScandinavianAE".to_owned()),
                name: Some("Scandinavian Aesthetics".to_owned()),
                avatar_url: None,
            }),
            media: None,
        }
    }

    fn tweet_example() -> FxTweet {
        FxTweet {
            id: Some("2050662857763688642".to_owned()),
            text: Some("No that's fine I just imagined it would be a bit bigger".to_owned()),
            created_timestamp: Some(1_777_751_099),
            author: Some(FxAuthor {
                screen_name: Some("AdolphsonFalkk".to_owned()),
                name: Some("Elise \u{1F352}\u{1F352}\u{2671}".to_owned()),
                avatar_url: Some("https://example.com/a.jpg".to_owned()),
            }),
            replies: Some(538),
            retweets: Some(9_057),
            likes: Some(296_573),
            views: Some(7_401_595),
            quote: Some(Box::new(quote_example())),
            media: None,
            translation: None,
        }
    }

    #[test]
    fn native_x_status_matching_is_opt_in() {
        let enabled = FxTwitterResolver {
            options: FxTwitterOptions {
                route_x_statuses: true,
                ..FxTwitterOptions::default()
            },
        };

        for url in [
            "https://x.com/a/status/1",
            "https://www.x.com/a/status/1",
            "https://twitter.com/a/statuses/1",
            "https://mobile.twitter.com/i/web/status/1",
        ] {
            assert!(enabled.matches(&u(url)), "should match {url}");
        }
        for url in [
            "https://x.com/home",
            "https://twitter.com/a",
            "https://notx.com/a/status/1",
        ] {
            assert!(!enabled.matches(&u(url)), "should NOT match {url}");
        }
    }

    #[test]
    fn fxtwitter_options_read_boolean_environment_values() {
        let options = FxTwitterOptions::from_reader(|name| match name {
            "FLUXER_UNFURL_X_VIA_FXTWITTER" => Some("yes".to_owned()),
            "FLUXER_UNFURL_FXTWITTER_SHOW_AUTHOR" => Some("false".to_owned()),
            "FLUXER_UNFURL_FXTWITTER_SHOW_STATS" => Some("0".to_owned()),
            "FLUXER_UNFURL_FXTWITTER_SHOW_FOOTER" => Some("on".to_owned()),
            _ => None,
        });
        assert_eq!(
            options,
            FxTwitterOptions {
                route_x_statuses: true,
                show_author: false,
                show_stats: false,
                show_footer: true,
            }
        );
    }

    #[test]
    fn caps_video_rendition_at_1080_short_edge() {
        let response: FxResponse = serde_json::from_str(
            r#"{
                "tweet": {
                    "media": {
                        "all": [{
                            "type": "video",
                            "url": "https://video.twimg.com/vid/2160x3840/original.mp4",
                            "thumbnail_url": "https://pbs.twimg.com/thumb.jpg",
                            "width": 2160,
                            "height": 3840,
                            "duration": 177.844,
                            "formats": [
                                {
                                    "url": "https://video.twimg.com/vid/720x1280/medium.mp4",
                                    "bitrate": 4096000,
                                    "container": "mp4",
                                    "codec": "h264"
                                },
                                {
                                    "url": "https://video.twimg.com/vid/1080x1920/preferred.mp4",
                                    "bitrate": 10368000,
                                    "container": "mp4",
                                    "codec": "h264"
                                },
                                {
                                    "url": "https://video.twimg.com/vid/2160x3840/too-large.mp4",
                                    "bitrate": 25128000,
                                    "container": "mp4",
                                    "codec": "h264"
                                }
                            ],
                            "variants": [
                                {
                                    "url": "https://video.twimg.com/vid/720x1280/medium.mp4",
                                    "bitrate": 4096000,
                                    "content_type": "video/mp4"
                                },
                                {
                                    "url": "https://video.twimg.com/vid/1080x1920/preferred.mp4",
                                    "bitrate": 10368000,
                                    "content_type": "video/mp4"
                                },
                                {
                                    "url": "https://video.twimg.com/vid/2160x3840/too-large.mp4",
                                    "bitrate": 25128000,
                                    "content_type": "video/mp4"
                                }
                            ]
                        }]
                    }
                }
            }"#,
        )
        .unwrap();
        let tweet = response.tweet.unwrap();
        let choice = first_choice_from_all(tweet.media.as_ref().unwrap()).unwrap();
        assert_video_choice(
            choice,
            "https://video.twimg.com/vid/1080x1920/preferred.mp4",
            Some("https://pbs.twimg.com/thumb.jpg"),
            Some(1080),
            Some(1920),
            Some(177.844),
        );
    }

    #[test]
    fn build_description_can_omit_social_stats() {
        assert_eq!(
            build_description(&tweet_example(), false),
            "No that's fine I just imagined it would be a bit bigger\n\n> **[Quoting](https://x.com/ScandinavianAE/status/2050646038420045877) Scandinavian Aesthetics \\([@ScandinavianAE](https://x.com/ScandinavianAE)\\)**\n> \u{FE00}\n> Sweden \u{1F1F8}\u{1F1EA}"
        );
    }
}
