# Guidance for Claude Code

## Git commit signing

- This repo signs all commits (`commit.gpgsign = true`). **Always sign via
  `gpg-agent`** — let gpg-agent supply the key/passphrase.
- **Never sign manually and never bypass signing.** Do not pass `--no-gpg-sign`,
  do not set `-c commit.gpgsign=false`, and do not invoke `gpg` by hand to
  produce a signature.
- If a commit fails to sign (e.g. expired key, no pinentry/TTY, cancelled
  passphrase prompt), **stop and surface the error** to the user instead of
  committing unsigned. The user resolves the gpg-agent/key issue; then retry.
- The signing key is chosen by git's default (currently
  `git@hollensbe.org`, key `D2DA68C87110DB25`). An older key
  (`A0EF131D0BAC3BF1`) is **expired** — don't select it.
