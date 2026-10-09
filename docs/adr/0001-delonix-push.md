# ADR-0001 — delonix-push: notificações push da N'GolaCloud

**Estado:** Proposto · **Data:** 2026-10-09 · **Origem:** ADR-0023 do delonix-meet (acordar o DelonixPhone)

## Decisão

Um serviço próprio, com os princípios do Firebase Cloud Messaging: **projectos** com chave de servidor, **aparelhos**
com segredo próprio, **mensagens só de dados** com prioridade, TTL, `collapse_key` e idempotência, **tópicos**,
estado de entrega consultável. A entrega vai por três caminhos escolhidos por mensagem:

1. **Ligação própria** (WebSocket, `GET /v1/connect`): a única com *ack* e sem terceiros. Pelo menos uma vez;
   a app deduplica por `id`. Sem *ack* em `ack_timeout`, reenvia.
2. **FCM** (Android) e 3. **APNs** (iOS, incluindo `voip`), atrás do *trait* `Provider`: servem para acordar uma
   app que não tem ligação viva. Só garantem «aceite pelo fornecedor» (estado `accepted`).

Sem ligação viva nem token de fornecedor a mensagem **fica na fila** até ao TTL (por omissão e no máximo, 28 dias)
e é entregue quando o aparelho ligar.

## O que isto NÃO resolve (e ninguém resolve sem terceiros)

- **iPhone:** só a Apple acorda uma app morta. Sem APNs não há push no iOS; o caminho 1 só serve com a app aberta.
- **Android:** a ligação própria exige um serviço em primeiro plano com notificação permanente e sofre com Doze e
  com as políticas de bateria de cada fabricante. É o que o UnifiedPush e o ntfy fazem. O FCM é mais fiável e
  poupa bateria. Por isso o desenho é híbrido: o FCM acorda, a ligação própria entrega com *ack*.
- Os adaptadores FCM e APNs **nunca falaram com a Google nem com a Apple** (sem contas). Foram medidos contra
  servidores de papel que verificam a assinatura do JWT (RS256/ES256), os cabeçalhos e o corpo.

## Modelo de segurança

- Chave de servidor `dpk_…` e segredo de aparelho `dpd_…`: 256 bits aleatórios, **só o SHA-256 está na base**.
- Todas as consultas levam `project_id`: um projecto nunca vê aparelhos, tópicos nem mensagens de outro
  (teste com dois projectos, mutação apanhada).
- Quem regista aparelhos é o servidor da app (chave de servidor), que entrega o segredo ao aparelho; não há registo
  público anónimo. O aparelho só pode mudar o seu próprio token de fornecedor, e só do fornecedor da sua plataforma.
- O conteúdo de uma mensagem é do remetente: não pôr credenciais nem dados pessoais no *payload*.
- Administração (criar projectos) por `X-Admin-Token`; vazio = fechada.

## Limites conhecidos da v1

- **Uma instância de gateway:** o registo de ligações vive em memória. Com várias instâncias, uma mensagem só chega
  pela ligação quando o *worker* da instância certa a apanhar (o `claim` com lease evita duplicados, mas não
  encaminha). Próximo passo: `LISTEN/NOTIFY` ou um bus.
- Difusão de tópico limitada a 1000 aparelhos por pedido, síncrona.
- Sem limite de taxa por projecto, sem métricas, sem TLS próprio (atrás do edge), sem chaves de cliente públicas.
- `payload` ≤ 4096 bytes. FCM só aceita strings: o *payload* vai serializado em `data.payload`.
