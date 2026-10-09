-- Que instância do servidor tem a ligação viva de cada aparelho (várias réplicas). Uma linha por aparelho; uma
-- ligação nova noutra instância substitui a anterior. `seen_at` renova-se no heartbeat: uma linha velha (instância
-- que morreu) deixa de contar.
CREATE TABLE device_connections (
    device_id    uuid PRIMARY KEY REFERENCES devices(id) ON DELETE CASCADE,
    node_id      uuid NOT NULL,
    connected_at timestamptz NOT NULL DEFAULT now(),
    seen_at      timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX device_connections_node ON device_connections(node_id);
