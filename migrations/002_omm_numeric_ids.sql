UPDATE songs
SET omm_id = NULL, omm_checked_at = NULL, omm_retry_at = NULL, omm_synced_at = NULL
WHERE omm_id IS NOT NULL
  AND NOT (
    omm_id GLOB 'omm:song:[1-9]*'
    AND omm_id NOT GLOB 'omm:song:*[^0-9]*'
    AND length(omm_id) <= 27
  );

UPDATE albums
SET omm_id = NULL, omm_checked_at = NULL, omm_retry_at = NULL, omm_art_url = NULL, omm_art_tried_at = NULL
WHERE omm_id IS NOT NULL
  AND NOT (
    omm_id GLOB 'omm:album:[1-9]*'
    AND omm_id NOT GLOB 'omm:album:*[^0-9]*'
    AND length(omm_id) <= 28
  );

UPDATE artists
SET omm_id = NULL, omm_checked_at = NULL, omm_retry_at = NULL, omm_art_url = NULL, omm_art_tried_at = NULL
WHERE omm_id IS NOT NULL
  AND NOT (
    omm_id GLOB 'omm:artist:[1-9]*'
    AND omm_id NOT GLOB 'omm:artist:*[^0-9]*'
    AND length(omm_id) <= 29
  );
