// SPDX-License-Identifier: AGPL-3.0-or-later

import type {MessageEmbed} from '@fluxer/schema/src/domains/message/EmbedSchemas';

export interface FiresharePlayer {
	url: string;
	posterUrl: string;
}

/**
 * Keep iframe playback limited to media from this instance's Fireshare host.
 * The unfurler has already checked public video metadata; this second check
 * prevents an unrelated embed from turning into an arbitrary iframe.
 */
export function getFiresharePlayer(embed: Pick<MessageEmbed, 'url' | 'video'>): FiresharePlayer | null {
	if (!embed.url || !embed.video?.url) return null;

	try {
		const share = new URL(embed.url);
		const media = new URL(embed.video.url);
		if (share.origin !== 'https://clips.dkzver.com' || media.origin !== share.origin) return null;
		const match = /^\/(?:w|watch)\/([A-Za-z0-9_-]+)\/?$/.exec(share.pathname);
		if (!match) return null;

		const id = match[1];
		const original = new RegExp('^/_content/video/' + id + '\\.(mp4|m4v|mov|webm)$');
		const derived = new RegExp('^/_content/derived/' + id + '/' + id + '-(480|720|1080)p\\.mp4$');
		if (!original.test(media.pathname) && !derived.test(media.pathname)) return null;

		return {
			url: share.origin + '/embed/' + id,
			posterUrl: share.origin + '/_content/derived/' + id + '/poster.jpg',
		};
	} catch {
		return null;
	}
}
