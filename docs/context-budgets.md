# Reserve context for difficult work

A context reservation temporarily raises the input threshold for one scope. Use
it while assembling evidence across several systems, then release it when that
phase ends. The proxy returns the budget it can honor; declaring a capacity does
not change the provider's or client's supported window.

## Start a scoped client

```sh
gobstopper proxy run --context-window 1000000 --client-context-window 1000000 --adaptive-context -- claude
```

Use capacities supported by your selected route and client. `proxy run` creates a
random scope capability, binds the child's provider URLs to it, and sets
`GOBSTOPPER_SCOPE`. Child workers inheriting that environment share the scope and
its request allowance. Start separate wrappers when workers need independent
budgets. No provider session identifier is inferred from this scope.

An agent running inside the wrapper can request a larger working set:

```sh
gobstopper context reserve --tokens 500000 --requests 20 --ttl-seconds 1800
gobstopper context status
gobstopper context release
```

All commands return JSON. `requested_input_tokens` is the requested budget;
`effective_input_tokens` is the granted threshold, with `limiting_reason` explaining
any reduction. Unknown capacity cannot authorize a larger threshold. The effective
input capacity uses the smallest configured provider/client window and reserves
at least the configured output headroom (32,000 by default), or the request's
larger output limit. Token estimates are calibrated from provider usage where
available. An immutable head or required recent turns exceeding an explicit
capacity cause a local error; retry and original-body fallback cannot bypass it.

A reservation ends when its request allowance or TTL expires, when released, or
when its scope is closed. Retries consume the same logical request allowance.
Concurrent requests use transactional accounting; an older adaptive observation
cannot overwrite a newer explicit reservation. State survives proxy restart.
Reservations have finite request, lifetime and size limits. Up to 4,096 scopes
can exist in one store. Scopes persist until explicitly closed; abandoned scopes
aren't silently deleted because a surviving client may still use them. Reaching
the scope limit reports an error. Keep a scope's capability until every client
using it has stopped, then close it.

## Use an existing proxy

```sh
gobstopper context create --context-window 1000000 --client-context-window 1000000 --adaptive --json
```

This command defaults to port 8260. Add `--port <port>` when your existing proxy
uses a different port; it does not infer the port from the installed service.

Use the returned `base_url` in your client's provider configuration, preserving
that client's usual endpoint suffix. Alternatively attach `X-Gobstopper-Scope`
with the returned capability. Set `GOBSTOPPER_SCOPE` in that client's environment
or pass `--scope` to context commands. The scope token authorizes that scope's
controls: keep it private. Gobstopper removes the routing prefix and header before
forwarding upstream, and stores only its hash. A request naming an unavailable
scope fails explicitly. If context storage was unavailable at proxy startup,
scoped requests retry opening it at most once every 30 seconds. They continue to
fail explicitly until the existing state can be read safely; recovery preserves
reservations and their remaining request allowances.

After every client using it has stopped, run
`gobstopper context close --scope <capability>`.

## Give a long-lived agent a standing threshold

A reservation suits a phase; it ends by request count or time. An agent that
should always run wider than the proxy default — a coordinator holding one
session for days — needs a threshold that belongs to its scope rather than to a
reservation that must be re-armed:

```sh
gobstopper context create --context-window 1000000 --client-context-window 1000000 --threshold 512000 --json
```

After upgrading Gobstopper, restart any running proxy before using a standing
threshold.

Requests in that scope compact at the scope's threshold instead of
`--threshold` or `--threshold-1m`, and `limiting_reason` reports
`scope_threshold`. The same capacity rules apply: a threshold above the
declared input capacity is clipped to it, and with no declared capacity only a
threshold below the proxy's own takes effect. A reservation still wins while it
is live; when it expires or is released, the scope returns to its standing
threshold rather than the proxy default. `context status` reports the standing
value as `scope_threshold_tokens`. Keep it below the client's own
auto-compaction point.

## Optional automatic rescue

`--adaptive-context` on `proxy run`, or `--adaptive` on `context create`, enables a
bounded response to repeated rereads of unchanged evidence that the proxy
observed and then evicted. It requires an explicit scope and configured capacity.
Historical messages count once; changed results do not count as unchanged reads.
Remote image URLs alone cannot prove that the image bytes are unchanged.

After three qualifying rereads within 15 minutes, the next requests can receive
up to twice the current threshold, bounded by input capacity, for eight requests
or ten minutes. A cooldown limits repeated rescue. An explicit reservation takes
priority. This detector identifies evidence churn, not task failure: polling,
rechecking an image and revisiting a file can all be useful work.

Reservations work with [bounded evidence retention](context-retention.md).
Neither guarantees that every observation survives, that the client will avoid
its own compaction, or that a larger context improves task accuracy. Inspect
`proxy status`, `context status`, and [local metrics](session-data.md) to see the
applied policy and its effects.
