-- The SQLite adapter applies the schema rebuild after SQLx has recorded this
-- migration. SQLx 0.8.6 parses `-- no-transaction`, but its SQLite migrator
-- still executes every migration in a transaction, so changing
-- `PRAGMA foreign_keys` here would be ineffective and could discard child
-- rows on an existing database. See sqlite::migrate_operation_resource_scope.
SELECT 1;
