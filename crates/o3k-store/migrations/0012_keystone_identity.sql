CREATE TABLE IF NOT EXISTS keystone_domains (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    description TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS keystone_projects (
    id TEXT PRIMARY KEY,
    domain_id TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    FOREIGN KEY(domain_id) REFERENCES keystone_domains(id),
    UNIQUE(domain_id, name)
);

CREATE TABLE IF NOT EXISTS keystone_users (
    id TEXT PRIMARY KEY,
    domain_id TEXT NOT NULL,
    name TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    email TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    FOREIGN KEY(domain_id) REFERENCES keystone_domains(id)
);

CREATE TABLE IF NOT EXISTS keystone_roles (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    description TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS keystone_role_assignments (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    role_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY(user_id) REFERENCES keystone_users(id),
    FOREIGN KEY(project_id) REFERENCES keystone_projects(id),
    FOREIGN KEY(role_id) REFERENCES keystone_roles(id),
    UNIQUE(user_id, project_id, role_id)
);

CREATE TABLE IF NOT EXISTS keystone_services (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    type TEXT NOT NULL,
    description TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS keystone_endpoints (
    id TEXT PRIMARY KEY,
    service_id TEXT NOT NULL,
    interface TEXT NOT NULL,
    url TEXT NOT NULL,
    region TEXT NOT NULL DEFAULT 'RegionOne',
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    FOREIGN KEY(service_id) REFERENCES keystone_services(id)
);
