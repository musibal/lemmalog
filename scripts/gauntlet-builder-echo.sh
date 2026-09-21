#!/bin/sh
# Gauntlet builder "artifact-echo": reads the build packet on stdin and
# re-emits its artifact unchanged, so a rework cycle stops with builder_noop
# and the gate ends final "rework" (fail-closed; no auto-rework here).
# Packet shape (serde_json BTreeMap order, one compact line):
# {"artifact":{...},"case_id":"<uuid>","failed_dimension":"<str>","next_action":"<str>"}
# ponytail: sed assumes that exact key order and enum-ish last two fields;
# per Pere (2026-09-21) JEV is the only agent allowed in this container, so
# no auto-reviser: rework stays fail-closed for a human/caller revision.
sed -E 's/^\{"artifact"://; s/,"case_id":"[^"]*","failed_dimension":"[^"]*","next_action":"[^"]*"\}$//'
