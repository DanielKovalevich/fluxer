/*
 * Copyright (C) 2026 Fluxer Contributors
 *
 * This file is part of Fluxer.
 *
 * Fluxer is free software: you can redistribute it and/or modify
 * it under the terms of the GNU Affero General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * Fluxer is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
 * GNU Affero General Public License for more details.
 *
 * You should have received a copy of the GNU Affero General Public License
 * along with Fluxer. If not, see <https://www.gnu.org/licenses/>.
 */

import {beforeEach, describe, expect, it} from 'vitest';
import {isWaylandSession} from './wayland';

describe('isWaylandSession', () => {
	const originalPlatform = process.platform;

	beforeEach(() => {
		Object.defineProperty(process, 'platform', {value: originalPlatform, writable: true});
		delete process.env.XDG_SESSION_TYPE;
		delete process.env.WAYLAND_DISPLAY;
	});

	function setPlatform(platform: string) {
		Object.defineProperty(process, 'platform', {value: platform, writable: true});
	}

	it('returns false on non-linux platforms', () => {
		setPlatform('win32');
		process.env.XDG_SESSION_TYPE = 'wayland';
		expect(isWaylandSession()).toBe(false);

		setPlatform('darwin');
		expect(isWaylandSession()).toBe(false);
	});

	it('returns true when XDG_SESSION_TYPE is wayland', () => {
		setPlatform('linux');
		process.env.XDG_SESSION_TYPE = 'wayland';
		expect(isWaylandSession()).toBe(true);
	});

	it('returns true when WAYLAND_DISPLAY is set', () => {
		setPlatform('linux');
		process.env.WAYLAND_DISPLAY = 'wayland-0';
		expect(isWaylandSession()).toBe(true);
	});

	it('returns true when both XDG_SESSION_TYPE and WAYLAND_DISPLAY are set', () => {
		setPlatform('linux');
		process.env.XDG_SESSION_TYPE = 'wayland';
		process.env.WAYLAND_DISPLAY = 'wayland-0';
		expect(isWaylandSession()).toBe(true);
	});

	it('returns false when XDG_SESSION_TYPE is x11', () => {
		setPlatform('linux');
		process.env.XDG_SESSION_TYPE = 'x11';
		expect(isWaylandSession()).toBe(false);
	});

	it('returns false when no session env vars are set on linux', () => {
		setPlatform('linux');
		expect(isWaylandSession()).toBe(false);
	});

	it('returns false when XDG_SESSION_TYPE is tty', () => {
		setPlatform('linux');
		process.env.XDG_SESSION_TYPE = 'tty';
		expect(isWaylandSession()).toBe(false);
	});
});
