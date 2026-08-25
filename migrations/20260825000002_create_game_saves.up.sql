-- Game saves.
--
-- Each save belongs to one player and stores an arbitrary JSON blob plus
-- optional game-specific metadata (see docs/TALO_API.md → game saves).
-- Saves are addressed externally by `public_id` (UUID); the internal BIGINT
-- id never leaves the server, mirroring the `players` table convention.

CREATE TABLE IF NOT EXISTS game_saves (
    id          BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    public_id   CHAR(36)        NOT NULL,
    player_id   BIGINT UNSIGNED NOT NULL,
    name        VARCHAR(255)    NOT NULL,
    save        JSON            NOT NULL,
    metadata    JSON            NULL,
    created_at  DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at  DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP
                                ON UPDATE CURRENT_TIMESTAMP,

    PRIMARY KEY (id),
    UNIQUE KEY uk_game_saves_public_id (public_id),
    KEY idx_game_saves_player (player_id),

    CONSTRAINT fk_game_saves_player
        FOREIGN KEY (player_id) REFERENCES players (id)
        ON DELETE CASCADE
        ON UPDATE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
