-- Initial players table.
--
-- This is the canonical player/account table for the Playzoid server.
-- Subaccounts are modeled via the self-referencing `parent_account_id`
-- foreign key (see TaloRustServerPlan.md and docs/TALO_API.md). A row
-- whose `parent_account_id` is NULL is a root account; otherwise it is
-- a subaccount of the referenced player.

CREATE TABLE IF NOT EXISTS players (
    id                 BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    public_id          CHAR(36)        NOT NULL,
    username           VARCHAR(64)     NOT NULL,
    email              VARCHAR(255)    NULL,
    password_hash      VARCHAR(255)    NOT NULL,
    parent_account_id  BIGINT UNSIGNED NULL,
    status             ENUM('active','suspended','deleted')
                                       NOT NULL DEFAULT 'active',
    created_at         DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at         DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP
                                       ON UPDATE CURRENT_TIMESTAMP,
    deleted_at         DATETIME        NULL,

    PRIMARY KEY (id),
    UNIQUE KEY uk_players_public_id (public_id),
    UNIQUE KEY uk_players_username  (username),
    UNIQUE KEY uk_players_email     (email),
    KEY idx_players_parent_account_id (parent_account_id),
    KEY idx_players_status            (status),

    CONSTRAINT fk_players_parent_account
        FOREIGN KEY (parent_account_id) REFERENCES players (id)
        ON DELETE SET NULL
        ON UPDATE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
