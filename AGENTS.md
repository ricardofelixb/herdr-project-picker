# Global instructions

- Never push to a default branch, reset or clean unless the user asks. Ask before destructive
  infrastructure operations. Never print credentials.
- After successful merge, delete both remote and local branches.
- Look for an existing helper before writing one. Extract repeated code when sharing improves
  clarity or prevents drift; keep small one-off literals and shapes local.
- While coding, run only checks scoped to what you changed: its tests, lint on those files. CI owns
  the full run (whole-project type checks, builds, the full suite).
- Wait for every command to exit before summarizing it, report each stage's result, and never
  call a check passed unless it exited 0. A task isn't done while its checks fail.
- Prefer the smallest complete change and delete unnecessary code.
- A question is a request for an answer, not authorization to change code.
- Only wirte tests for sensitive flows or that may catch a meaningful regression.
- `~/.pi/agent` (settings, extensions, prompts, themes) and `~/.pi/acp.json` sync across the user's machines
  through the orchestrator repo's hourly `skills_link.py`: change them in place and the change spreads,
  deletions included. Logins, ids and machine paths stay local on their own; don't build another sync.
- Prefer webhooks over polling everytime. Avoid short lived polls at any costs.
- Do not use the browser unless told otherwise.
- When you use a Bigpowers skill, follow it where it differs from these rules, except that these
  always hold: never print credentials, ask before destructive infrastructure operations, and never
  push to a default branch, reset or clean unless the user asks. Inside a runner job, your role file wins.
