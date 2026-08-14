UPDATE apps
SET hostname = name || '.apps.joels.computer',
    generation = generation + 1,
    updated_at = now()
WHERE hostname <> name || '.apps.joels.computer';

ALTER TABLE apps
ADD CONSTRAINT apps_hostname_is_derived
CHECK (hostname = name || '.apps.joels.computer');
