-- Move every row through a collision-free temporary value so an existing app
-- named `lightning-app-foo` cannot block the row for `foo` during the rewrite.
UPDATE apps
SET namespace = 'lightning-migration-' || id::text;

UPDATE apps
SET namespace = 'lightning-app-' || name,
    generation = generation + 1,
    updated_at = now();

ALTER TABLE apps
ADD CONSTRAINT apps_name_fits_managed_namespace CHECK (char_length(name) <= 49),
ADD CONSTRAINT apps_namespace_is_derived CHECK (namespace = 'lightning-app-' || name);
