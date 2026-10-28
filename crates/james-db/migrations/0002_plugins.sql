-- Store scopes (see store.rs): plugin '' = shared by all plugins; scope '' = plugin-wide,
-- "instance:<id>" = per instance.
-- Plugin configuration per instance (values of the manifest's config fields).
CREATE TABLE plugin_instance_config (
    plugin   TEXT NOT NULL,
    instance TEXT NOT NULL,
    config   TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY (plugin, instance)
);

-- DJs may use plugin web pages and endpoints (`plugins.use`).
INSERT INTO role_permissions (role_id, permission)
    SELECT id, 'plugins.use' FROM roles WHERE name = 'dj';
