-- Per-game settings.
--
-- Each row holds one game's arbitrary JSON configuration (backing
-- GET/PUT /v1/games/{game_id}/settings — see docs/TALO_API.md → game config).
-- Games are addressed externally by their opaque `game_id` route identifier,
-- mirroring the leaderboards' `internal_name` convention; no `games` table
-- exists yet, so there is no foreign key. The internal BIGINT id never leaves
-- the server, mirroring the `players`/`game_saves` convention.
--
-- Size capping of `config` is deliberately not enforced here — MySQL JSON
-- columns cannot be size-bounded in schema. The cap is applied at the API
-- layer (task 0.4.4), the same split as the leaderboards' props ≤ 4 KB rule.

CREATE TABLE IF NOT EXISTS game_settings (
    id          BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    game_id     VARCHAR(64)     NOT NULL,
    config      JSON            NOT NULL,
    created_at  DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at  DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP
                                ON UPDATE CURRENT_TIMESTAMP,

    PRIMARY KEY (id),
    UNIQUE KEY uk_game_settings_game_id (game_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
