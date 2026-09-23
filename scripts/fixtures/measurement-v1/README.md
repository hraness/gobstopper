These four public synthetic cases and their span annotations were frozen before
running the study. `registration.json` pins every source, annotation, and control
by SHA-256. The runner records its own hash and a private copy of the candidate
executable before outcomes, then preserves every trial result, including failures.

Run from the repository root after building the candidate:

```sh
python3 scripts/registered-study.py --corpus-only
python3 scripts/registered-study.py --binary target/debug/gobstopper --output /tmp/gobstopper-measurement-new
```

The output directory must be new. No provider, model, credentials, or private
session is used. Each executed arm uses the same source, one round, a 64-token
target floor, and one protected recent output. Arms are an unchanged baseline,
ordinary masking, pinned masking, and a hybrid of masking with a verbatim card.
Provider native and provider native plus masking arms are registered as
**incomplete**; this runner has no activation path for them.

The cases cover negation, revoked approval, uncertain pending effects, and live
tool state. Each contradiction control preserves the old literal inside an
obsolete quote and replaces its meaning elsewhere. The expected literal score
remains positive. Passing that control demonstrates a limitation of substring
retention; it does not establish semantic equivalence or task success.

Reports separate bytes, normalized token estimates, literal and lexical presence,
source binding, local transformation time, and descriptive Wilson intervals.
Charged usage, cache hits, refetches, provider continuation, and task outcomes
remain unavailable. These related fixtures are not independent tasks, a blinded
annotation study, or a representative population sample. A successful runner exit
admits offline mechanics only; it never qualifies the missing native or semantic
comparison.

The process runner reserves the child leader's PID until group cleanup and bounds
its pipes and deadline. Its trusted executable assumption excludes privilege
changes and detached descendants; it is not an operating-system sandbox.
