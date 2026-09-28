// SPDX-License-Identifier: AGPL-3.0-or-later

import styles from '@app/features/channel/components/embeds/media/EmbedFireshare.module.css';
import type {FiresharePlayer} from '@app/features/channel/components/embeds/media/FireshareEmbedUtils';
import ActiveIframeEmbed from '@app/features/messaging/state/ActiveIframeEmbed';
import {openExternalUrlWithWarning} from '@app/features/messaging/utils/ExternalLinkUtils';
import {
	EMBED_MAX_HEIGHT,
	EMBED_MAX_WIDTH,
	EMBED_TALL_MAX_HEIGHT,
} from '@app/features/messaging/utils/MediaDimensionConfig';
import {remFromPx} from '@app/features/theme/layout/RemFromPx';
import {createCalculator, mediaAspectRatioValue} from '@app/features/ui/utils/DimensionUtils';
import type {MessageEmbed} from '@fluxer/schema/src/domains/message/EmbedSchemas';
import {msg} from '@lingui/core/macro';
import {useLingui} from '@lingui/react/macro';
import {ArrowSquareOutIcon, PlayIcon} from '@phosphor-icons/react';
import {observer} from 'mobx-react-lite';
import {type FC, useCallback, useEffect, useId, useState} from 'react';

const PLAY_VIDEO = msg({message: 'Play Fireshare video'});
const OPEN_CLIP = msg({message: 'Open clip in Fireshare'});
const PLAYER_NAME = msg({message: 'Fireshare video player'});

const calculator = createCalculator({
	maxWidth: EMBED_MAX_WIDTH,
	maxHeight: EMBED_MAX_HEIGHT,
	responsive: true,
});

interface EmbedFireshareProps {
	embed: MessageEmbed;
	player: FiresharePlayer;
}

export const EmbedFireshare: FC<EmbedFireshareProps> = observer(({embed, player}) => {
	const {i18n} = useLingui();
	const embedId = useId();
	const active = ActiveIframeEmbed.isActive(embedId);
	const [posterFailed, setPosterFailed] = useState(false);
	const width = embed.video?.width ?? 1280;
	const height = embed.video?.height ?? 720;
	const {dimensions} = calculator.calculate(
		{width, height},
		{maxWidth: EMBED_MAX_WIDTH, maxHeight: height > width ? EMBED_TALL_MAX_HEIGHT : EMBED_MAX_HEIGHT},
	);

	const openClip = useCallback((event: React.MouseEvent) => {
		event.stopPropagation();
		if (embed.url) openExternalUrlWithWarning(embed.url);
	}, [embed.url]);

	useEffect(() => () => ActiveIframeEmbed.release(embedId), [embedId]);

	return (
		<div
			className={styles.container}
			style={{
				width: remFromPx(dimensions.width),
				aspectRatio: mediaAspectRatioValue(dimensions),
				maxWidth: '100%',
			}}
			data-flx="channel.embeds.media.embed-fireshare.container"
		>
			{active ? (
				<>
					{/* biome-ignore lint/a11y/useIframeTitle: project policy uses aria-label for iframe names */}
					<iframe
						src={player.url + '?autoplay=1'}
						allow="autoplay; fullscreen"
						allowFullScreen
						sandbox="allow-same-origin allow-scripts"
						aria-label={i18n._(PLAYER_NAME)}
						className={styles.iframe}
						data-embed-media="true"
						data-flx="channel.embeds.media.embed-fireshare.iframe"
					/>
				</>
			) : (
				<>
					{!posterFailed && (
						<img
							src={embed.thumbnail?.proxy_url || player.posterUrl}
							alt=""
							loading="lazy"
							className={styles.poster}
							onError={() => setPosterFailed(true)}
							data-flx="channel.embeds.media.embed-fireshare.poster"
						/>
					)}
					<button
						type="button"
						className={styles.play}
						onClick={(event) => {
							event.stopPropagation();
							ActiveIframeEmbed.claim(embedId);
						}}
						aria-label={i18n._(PLAY_VIDEO)}
						data-flx="channel.embeds.media.embed-fireshare.play"
					>
						<PlayIcon size={32} weight="fill" aria-hidden="true" />
					</button>
					<button
						type="button"
						className={styles.open}
						onClick={openClip}
						aria-label={i18n._(OPEN_CLIP)}
						data-flx="channel.embeds.media.embed-fireshare.open"
					>
						<ArrowSquareOutIcon size={20} aria-hidden="true" />
					</button>
				</>
			)}
		</div>
	);
});
