-- Limites por projecto, definidos pelo operador da plataforma (NULL = os valores por omissão do servidor).
ALTER TABLE projects ADD COLUMN rate_per_sec int CHECK (rate_per_sec IS NULL OR rate_per_sec > 0);
ALTER TABLE projects ADD COLUMN daily_quota bigint CHECK (daily_quota IS NULL OR daily_quota > 0);
