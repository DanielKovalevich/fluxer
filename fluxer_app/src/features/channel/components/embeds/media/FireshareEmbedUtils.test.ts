// SPDX-License-Identifier: AGPL-3.0-or-later

import type {MessageEmbed} from '@fluxer/schema/src/domains/message/EmbedSchemas';
import {describe, expect, it} from 'vitest';
import {getFiresharePlayer} from './FireshareEmbedUtils';

const clipId = '148ce53f86639176dac0a7991555df90';
const host = 'https://clips.dkzver.com';

function embed(url: string, videoUrl: string): Pick<MessageEmbed, 'url' | 'video'> {
	return {url, video: {url: videoUrl, flags: 0}};
}

describe('getFiresharePlayer', () => {
	it('recognizes existing Fireshare watch embeds without changing their video payload', () => {
		expect(
			getFiresharePlayer(embed(
				host + '/w/' + clipId,
				host + '/_content/derived/' + clipId + '/' + clipId + '-720p.mp4',
			)),
		).toEqual({
			url: host + '/embed/' + clipId,
			posterUrl: host + '/_content/derived/' + clipId + '/poster.jpg',
		});
		expect(
			getFiresharePlayer(embed(host + '/watch/' + clipId, host + '/_content/video/' + clipId + '.mp4')),
		).not.toBeNull();
	});

	it('rejects lookalike hosts, unrelated videos, and malformed share paths', () => {
		const direct = host + '/_content/video/' + clipId + '.mp4';
		for (const [share, media] of [
			['https://clips.dkzver.com.evil.example/w/' + clipId, direct],
			['http://clips.dkzver.com/w/' + clipId, direct],
			[host + '/w/' + clipId + '/other', direct],
			[host + '/w/' + clipId, 'https://other.example/video.mp4'],
			[host + '/w/' + clipId, host + '/_content/video/another.mp4'],
		]) {
			expect(getFiresharePlayer(embed(share, media))).toBeNull();
		}
	});
});
