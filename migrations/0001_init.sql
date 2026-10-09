-- delonix-push: projectos (as «apps»), chaves de servidor, aparelhos, subscrições e mensagens.
CREATE TABLE projects (
    id          uuid PRIMARY KEY,
    name        text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);

-- A chave de servidor só existe em claro uma vez, na criação; guarda-se o SHA-256.
CREATE TABLE project_keys (
    id          uuid PRIMARY KEY,
    project_id  uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    key_hash    bytea NOT NULL UNIQUE,
    label       text NOT NULL DEFAULT '',
    created_at  timestamptz NOT NULL DEFAULT now(),
    revoked_at  timestamptz
);

CREATE TABLE devices (
    id             uuid PRIMARY KEY,
    project_id     uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    platform       text NOT NULL CHECK (platform IN ('android','ios')),
    -- Como se acorda/entrega: 'direct' (ligação própria), 'fcm', 'apns'.
    provider       text NOT NULL DEFAULT 'direct' CHECK (provider IN ('direct','fcm','apns')),
    provider_token text,
    secret_hash    bytea NOT NULL UNIQUE,
    created_at     timestamptz NOT NULL DEFAULT now(),
    last_seen_at   timestamptz,
    revoked_at     timestamptz
);
CREATE INDEX devices_project ON devices(project_id) WHERE revoked_at IS NULL;

CREATE TABLE subscriptions (
    device_id  uuid NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    topic      text NOT NULL,
    PRIMARY KEY (device_id, topic)
);
CREATE INDEX subscriptions_topic ON subscriptions(project_id, topic);

CREATE TABLE messages (
    id              uuid PRIMARY KEY,
    project_id      uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    device_id       uuid NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    collapse_key    text,
    priority        text NOT NULL CHECK (priority IN ('high','normal')),
    payload         jsonb NOT NULL,
    idempotency_key text,
    state           text NOT NULL CHECK (state IN ('queued','sent','delivered','accepted','expired','failed')),
    attempts        int NOT NULL DEFAULT 0,
    last_error      text,
    created_at      timestamptz NOT NULL DEFAULT now(),
    expires_at      timestamptz NOT NULL,
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    sent_at         timestamptz,
    delivered_at    timestamptz
);
CREATE UNIQUE INDEX messages_idem ON messages(project_id, device_id, idempotency_key) WHERE idempotency_key IS NOT NULL;
CREATE INDEX messages_due ON messages(next_attempt_at) WHERE state IN ('queued','sent');
CREATE INDEX messages_device_open ON messages(device_id, created_at) WHERE state IN ('queued','sent');
