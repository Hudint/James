-- Names set in the web interface; they win over the file's tags and survive rescans.
-- NULL = the file's value; '' for artist/album = explicitly none.
ALTER TABLE library_tracks ADD COLUMN custom_title TEXT;
ALTER TABLE library_tracks ADD COLUMN custom_artist TEXT;
ALTER TABLE library_tracks ADD COLUMN custom_album TEXT;
