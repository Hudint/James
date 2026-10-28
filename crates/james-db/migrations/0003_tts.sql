-- DJs may use text to speech (`!say`, the web interface and the API).
INSERT INTO role_permissions (role_id, permission)
    SELECT id, 'tts.say' FROM roles WHERE name = 'dj';
