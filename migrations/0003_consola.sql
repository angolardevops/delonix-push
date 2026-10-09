-- Plano de controlo (ADR-0002): contas que entram na consola, organizações (equipas) e papéis. Um projecto passa a
-- pertencer a uma organização; os criados pela API de administração antiga ficam sem organização (org_id NULL).
CREATE TABLE accounts (
    id            uuid PRIMARY KEY,
    email         text NOT NULL UNIQUE CHECK (email = lower(email) AND char_length(email) <= 254),
    password_hash text NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE organizations (
    id         uuid PRIMARY KEY,
    name       text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 100),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE memberships (
    account_id uuid NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    org_id     uuid NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    role       text NOT NULL CHECK (role IN ('owner','admin','developer','viewer')),
    PRIMARY KEY (account_id, org_id)
);
CREATE INDEX memberships_org ON memberships(org_id);

ALTER TABLE projects ADD COLUMN org_id uuid REFERENCES organizations(id) ON DELETE CASCADE;
CREATE INDEX projects_org ON projects(org_id);

-- Sessões da consola: token opaco `dps_…`, só o SHA-256 fica na base.
CREATE TABLE console_sessions (
    token_hash bytea PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);
CREATE INDEX console_sessions_account ON console_sessions(account_id);

-- Auditoria das acções de gestão.
CREATE TABLE audit_log (
    id         bigserial PRIMARY KEY,
    org_id     uuid,
    project_id uuid,
    account_id uuid,
    action     text NOT NULL,
    detail     text NOT NULL DEFAULT '',
    at         timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX audit_log_org ON audit_log(org_id, at DESC);

ALTER TABLE project_keys ADD COLUMN created_by uuid;
