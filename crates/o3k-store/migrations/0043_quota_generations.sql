ALTER TABLE quota_limits ADD COLUMN generation INTEGER NOT NULL DEFAULT 0 CHECK (generation >= 0);
