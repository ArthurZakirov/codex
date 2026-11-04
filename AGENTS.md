# Codex fork: agent and voice-release workflow

This repository is an upstream Codex fork. Its **portable macOS Codex Voice** distribution is maintained separately from upstream Codex's standard CLI.

## Scope and source of truth

- For changes to realtime voice, local speech, voice packaging, or the portable Voice release, use the current `feat/portable-codex-voice-release` branch as the integration base. Check its current remote HEAD before editing or building. Do not assume `main` already includes the Voice implementation.
- Do feature work in a separate linked worktree at `<permanent-checkout>.worktrees/<branch-slug>`. Never overwrite another worktree's dirty changes. Keep the permanent checkout on its remote default branch.
- These instructions augment the user's global `~/.codex/AGENTS.md`; never modify its generated copy directly.
- Release source of truth: [portable Voice workflow](.github/workflows/portable-codex-voice.yml). Release consumer: `ArthurZakirov/dotfiles`, file `home/.chezmoidata.toml`, section `[codexVoice]`. Discover its actual location instead of assuming a particular home directory.

## Voice behavior to preserve

- With the local verbatim option active and an active Voice session, speak **every completed, visible assistant message**: commentary/progress and final answers, whether the user spoke or typed. Do not speak private reasoning, tool logs, or replayed historical messages.
- Local speech uses one ordered queue. New typed input or microphone transcription must not discard a previously queued message.
- On macOS, **Space pauses/resumes the currently speaking local synthesizer**, without losing its position, canceling the Voice session, or dropping queued messages. Preserve normal Space input when the composer contains text, no local speech is active, or a dialog/transcript interaction owns the key. Ignore key-repeat toggles.
- The separate `codex-voice` launcher should enable `CODEX_REALTIME_VERBATIM_SPEECH=1` by default; avoid modifying the stock `codex` command.
- Preserve the release's independent normal `codex`, existing voice experiments, and voice helper/native-runtime verification.

## Voice change: required end-to-end completion

For each user-requested Voice feature or fix, do not call work complete merely because a PR was merged. Carry the work through these gates, unless the user explicitly requests code-only changes or a gate is blocked. Report each gate's actual result.

1. **Implement and test.** Run Rustfmt, the targeted `codex-tui` typecheck, relevant focused unit/integration tests, and macOS process tests for speech controls. Test the relevant voice lifecycle and compatibility with the FIFO queue. An interactive microphone/listening test is required before claiming that audio behavior was verified; automated tests alone do not establish that.
2. **Review and merge.** Create a focused PR into `feat/portable-codex-voice-release`; review actionable comments; monitor CI; repair failures without disabling the check that detects them. Merge only after the appropriate tests and review gates pass, respecting the user's explicit merge instructions.
3. **Produce a new immutable release.** The existing [release workflow](.github/workflows/portable-codex-voice.yml) builds on a push to the Voice release branch. Monitor the run and repair build/package failures. Never reuse or overwrite an existing `voice-v<version>-r<n>` tag. Do not claim a release exists until GitHub's Releases API confirms it has published both `aarch64-apple-darwin` and `x86_64-apple-darwin` archives and `SHA256SUMS`.
4. **Verify before promotion.** Ensure the published release points to the intended merged commit, artifacts and checksums match, native voice-helper signing/build provenance are valid, and the package runs in a disposable HOME. Check `codex-voice --version` and `codex-voice --no-daemon --help`; verify the voice behaviors interactively when possible. A missing or failing artifact is a release blocker, not a reason to change the Dotfiles tag.
5. **Promote to Dotfiles.** Only after a release is verified, update `[codexVoice].tag` in `ArthurZakirov/dotfiles/home/.chezmoidata.toml` in a **fresh Dotfiles feature worktree**. Keep `enabled = true`. Run the existing real-Chezmoi integration/system tests with an isolated HOME. Commit, push, open a Dotfiles PR, resolve review and CI failures, and merge only with the user's authorization and required gates. Never apply an unmerged Dotfiles feature worktree to a real HOME.
6. **Verify onboarding.** After Dotfiles promotion, `chezmoi update` should install or verify the matching release without unnecessary reinstallation; `codex-voice --no-daemon` should run on both Macs. If the other Mac is disconnected, report that its real installation remains unverified.

### Automation and permissions

- `AGENTS.md` is a **task policy for agents, not an event handler**. It cannot wake an agent after a merge or run a GitHub workflow by itself. GitHub Actions handles the build on a release-branch push; a connected agent performs the verified cross-repository Dotfiles promotion. For fully unattended promotion, configure a separately authorized cross-repository GitHub App/credential and a guarded event workflow. Do not assume `GITHUB_TOKEN` in Codex can write to Dotfiles.
- **Cost constraint:** never choose GitHub-hosted *larger* runners, private-repository billable runs, paid services, or modify billing/payment settings. Prefer standard hosted runners in public repositories, which GitHub documents as free; verify repository visibility and runner class before launching. Avoid unnecessary full upstream CI matrices.
- Use the authenticated GitHub/SSH identity already available, never print access tokens, never create or inspect billing methods, and never bypass failed release validation.
- If any step is blocked by credentials, absent runner capacity, failed checks, or interactive audio verification, complete what is safe, report the exact blocker and remaining step, and **never silently claim the latest code is already shipped**.
