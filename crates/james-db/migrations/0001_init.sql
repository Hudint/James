-- Accounts ------------------------------------------------------------------

CREATE TABLE users (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
    -- NULL for accounts that can only log in via OIDC or platform identity
    password_hash TEXT,
    is_admin      INTEGER NOT NULL DEFAULT 0,
    disabled      INTEGER NOT NULL DEFAULT 0,
    -- OIDC subject (issuer|sub), if linked
    oidc_subject  TEXT UNIQUE,
    created_at    INTEGER NOT NULL
);

CREATE TABLE roles (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL UNIQUE COLLATE NOCASE,
    description TEXT NOT NULL DEFAULT '',
    -- built-in roles cannot be deleted
    builtin     INTEGER NOT NULL DEFAULT 0
);

-- A permission granted by a role, globally (instance_id NULL) or for one instance.
-- Supports wildcards: '*' and 'prefix.*'.
CREATE TABLE role_permissions (
    role_id     INTEGER NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
    permission  TEXT NOT NULL,
    instance_id TEXT,
    UNIQUE (role_id, permission, instance_id)
);

CREATE TABLE user_roles (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role_id INTEGER NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
    PRIMARY KEY (user_id, role_id)
);

-- Platform identities (TeamSpeak UID, Discord user id) linked to accounts.
CREATE TABLE identities (
    platform   TEXT NOT NULL,
    uid        TEXT NOT NULL,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name       TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    PRIMARY KEY (platform, uid)
);

-- Roles granted to all members of a platform group (TS server group, Discord role).
CREATE TABLE group_roles (
    platform TEXT NOT NULL,
    group_id TEXT NOT NULL,
    role_id  INTEGER NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
    PRIMARY KEY (platform, group_id, role_id)
);

-- Login sessions and API tokens (only the SHA-256 of the token is stored).
CREATE TABLE tokens (
    token_hash TEXT PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind       TEXT NOT NULL, -- 'session' | 'api'
    name       TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    expires_at INTEGER,
    last_used  INTEGER
);

-- Instances -------------------------------------------------------------------

CREATE TABLE instances (
    id          TEXT PRIMARY KEY,
    config      TEXT NOT NULL, -- InstanceConfig as JSON
    credentials TEXT,          -- adapter credentials as JSON
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

-- Media -------------------------------------------------------------------------

CREATE TABLE playlists (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT NOT NULL,
    owner_id   INTEGER REFERENCES users(id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE playlist_entries (
    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    position    INTEGER NOT NULL,
    track       TEXT NOT NULL, -- Track as JSON
    PRIMARY KEY (playlist_id, position)
);

-- Local library index (filled by the scanner).
CREATE TABLE library_tracks (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    path        TEXT NOT NULL UNIQUE,
    title       TEXT NOT NULL,
    artist      TEXT,
    album       TEXT,
    duration_ms INTEGER,
    size        INTEGER NOT NULL,
    mtime       INTEGER NOT NULL,
    added_at    INTEGER NOT NULL
);
CREATE INDEX library_tracks_artist ON library_tracks(artist);
CREATE INDEX library_tracks_album ON library_tracks(album);

-- Measured integrated loudness per track locator (avoids level jumps on replay).
CREATE TABLE loudness (
    source     TEXT NOT NULL,
    locator    TEXT NOT NULL,
    lufs       REAL NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (source, locator)
);

-- Plugins & settings ---------------------------------------------------------------

-- Key-value store for plugins; scope is '' (global) or an instance id.
CREATE TABLE plugin_store (
    plugin TEXT NOT NULL,
    scope  TEXT NOT NULL DEFAULT '',
    key    TEXT NOT NULL,
    value  TEXT NOT NULL,
    PRIMARY KEY (plugin, scope, key)
);

CREATE TABLE plugins (
    id                  TEXT PRIMARY KEY,
    enabled             INTEGER NOT NULL DEFAULT 0,
    -- permissions from the manifest the admin confirmed
    granted_permissions TEXT NOT NULL DEFAULT '[]',
    config              TEXT NOT NULL DEFAULT '{}',
    -- instances the plugin is active on; NULL = all
    instances           TEXT
);

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Built-in roles ----------------------------------------------------------------------

INSERT INTO roles (name, description, builtin) VALUES
    ('admin', 'Full access to everything', 1),
    ('everyone', 'Granted to every client on the voice platform, linked or not', 1),
    ('dj', 'Controls playback and the queue', 1);

INSERT INTO role_permissions (role_id, permission)
    SELECT id, '*' FROM roles WHERE name = 'admin';
INSERT INTO role_permissions (role_id, permission)
    SELECT id, p.permission FROM roles,
        (SELECT 'playback.*' AS permission UNION ALL SELECT 'queue.*' UNION ALL SELECT 'playlist.*' UNION ALL SELECT 'library.read') p
    WHERE name = 'dj';
INSERT INTO role_permissions (role_id, permission)
    SELECT id, p.permission FROM roles,
        (SELECT 'playback.play' AS permission UNION ALL SELECT 'playback.control' UNION ALL SELECT 'playback.volume' UNION ALL SELECT 'queue.manage') p
    WHERE name = 'everyone';
