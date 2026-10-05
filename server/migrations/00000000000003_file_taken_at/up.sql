-- When the photo/video was taken (ms since epoch), as the uploading device
-- knew it (Android: MediaStore DATE_TAKEN). A property of the content, like
-- width/height: the first upload sets it. NULL for files uploaded before this
-- column, or with no known capture time; listings fall back to added_at.
ALTER TABLE file ADD COLUMN taken_at INTEGER;
