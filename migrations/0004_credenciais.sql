-- Credenciais dos fornecedores por projecto (FCM, APNs), cifradas em repouso (AES-256-GCM, ver `seal`).
-- `meta` guarda só o que é seguro mostrar na consola (nunca a chave privada).
CREATE TABLE project_credentials (
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    provider   text NOT NULL CHECK (provider IN ('fcm','apns')),
    sealed     bytea NOT NULL,
    meta       jsonb NOT NULL DEFAULT '{}',
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, provider)
);
