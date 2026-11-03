# Installed CLI contract research

Recorded **2026-10-24T21:28:02Z** from the system clock. Public-documentation research only, not qualification of the installed clients. No installed executable, authentication flow, credential store, inference request or native UI was exercised.

## Decision

**Do not enable the installed-CLI prompt adapter yet.** Passive discovery remains available. A detected executable is neither authorization to execute it nor proof of a prompt-only environment. This is a bounded unresolved contract, not a conclusion that the product direction is impossible.

The current prompt-only browser promise is narrower than an ordinary coding-agent session: only explicitly supplied conversation text, no implicit project documents/memory, no tools or MCP connections, no hooks/plugins/background tasks, no inherited conversation, no credential extraction, no interactive fallback and bounded process-tree cleanup. Read-only filesystem sandboxing alone does not establish those properties.

## Claude Code — official documentation findings

This research examined exactly five official pages. Current flags must still be checked against the exact installed version before any eventual adapter qualification. Documentation describes version-specific behavior, not the bytes of any installed copy.

| Boundary | Documented behavior | Consequence for Crossbow |
| --- | --- | --- |
| Built-in tools | `--tools ""` disables built-ins; it does not affect MCP tools. | An actual empty argument is required. It is not sufficient by itself. |
| MCP model tools | `--disallowedTools "mcp__*"` removes all MCP tools; `"*"` removes every tool. | Tool removal does not establish that MCP processes never start/connect. |
| MCP configuration | `--strict-mcp-config` uses only explicitly supplied MCP configuration. A readable/parseable managed MCP file causes startup refusal rather than bypass. | Fail closed. Never retry after removing the isolation flag. |
| Implicit context/customization | `--bare` skips hooks, skills, custom commands, subagents, installed plugins, MCP discovery, auto memory and CLAUDE.md. Bare mode alone still has tools; `--add-dir` can add skills. Version2.1.286+ strengthens command-line-only MCP, reminder and background-task limits. | No `--add-dir`, no assumption that older bare mode has the same contract; managed-setting combination precedence remains unresolved. |
| Existing subscription login | Headless docs state: “In bare mode, Claude Code never reads OAuth credentials or the system keychain.” Bare mode also ignores `CLAUDE_CODE_OAUTH_TOKEN` and Anthropic profiles/federation credentials. First-party API access requires an API key or explicit key helper. | The documented isolated mode does not reuse ordinary subscription login. Crossbow must not extract/copy credentials or silently switch to billed API access. |
| Safe mode | Authentication remains ordinary, but managed policy can still supply hooks, status-line and file-suggestion commands. | Not a complete zero-implicit-execution mode while preserving existing login. |
| Restricted mode | Loads managed settings and explicit `--settings`; not settings-free or tool-free. | Not a substitute for the required boundary. |
| Settings sources | The reviewed docs list `user`, `project`, `local`; they did not establish an empty-string isolation contract or a way to disable managed policy. | Do not invent or rely on `--setting-sources ""` as a verified isolation switch. |
| Permission prompts | `--permission-prompts none` denies unresolved prompts, but rules/mode/hooks act first and a permission hook can allow. | Prompt suppression is not tool isolation. |

The unresolved combination is **existing subscription authentication plus fully suppressed implicit context/hooks/tools/MCP**. Do not weaken the browser's data promise to hide that difference. An eventual explicitly authorized API-key route is a different integration, not automatic subscription reuse.

### Official sources examined

- [CLI reference](https://code.claude.com/docs/en/cli-reference)
- [Headless and bare mode](https://code.claude.com/docs/en/headless.md#start-faster-with-bare-mode)
- [Authentication](https://code.claude.com/docs/en/authentication.md#generate-a-long-lived-token)
- [Managed MCP](https://code.claude.com/docs/en/managed-mcp.md#servers-passed-with---mcp-config-or---strict-mcp-config)
- [Settings precedence](https://code.claude.com/docs/en/settings.md#settings-precedence)

### Conversation, output and cancellation limits

- `--no-session-persistence` is print-mode-only and prevents resumable session files. It does **not** establish no diagnostic logs, telemetry, provider retention or other local writes. Do not pass resume/continue/fork/transcript/existing-session/cloud attachment inputs; forking inherits prior conversation rather than isolating it.
- `--system-prompt` replaces the default system prompt and the headless page says it removes the initial git-status snapshot. `--exclude-dynamic-system-prompt-sections` instead moves per-user context into the first user message; it does not remove that context.
- JSON output is an envelope with result/session metadata. With `--json-schema`, validate the presence and value of `structured_output`; refusals and incomplete runs can omit it. Schema `format` is annotation-only. Stream JSON needs `--verbose --include-partial-messages`; a final result event is distinct from partial text.
- `system/init` reports tools, servers and plugins, but startup hooks or plugin-install events can occur before it. Empty init lists do not prove no earlier side effects.
- `--max-turns` has no default; queued stream-JSON inputs can each get their own limit. `--max-budget-usd` is estimated and can overshoot, not a hard spend ceiling. Documented piped stdin cap:10 MB. A browser adapter would need tighter independent input/output/time bounds.
- On SIGTERM, print mode exits143, leaves the in-progress turn unfinished with no result, terminates running Bash process trees, then runs SessionEnd hooks. It starts no new tool/model call while exiting. This is not a guarantee that every detached descendant or remote MCP job stops; the docs explicitly state remote work may continue. Do not substitute this partial contract for measured process-tree cancellation.

Additional sections of the same five examined pages: [system prompt](https://code.claude.com/docs/en/headless.md#customize-the-system-prompt), [structured output](https://code.claude.com/docs/en/headless.md#get-structured-output), [streaming](https://code.claude.com/docs/en/headless.md#stream-responses), [session metadata](https://code.claude.com/docs/en/headless.md#read-session-metadata), [stdin](https://code.claude.com/docs/en/headless.md#pipe-data-through-claude), [SIGTERM](https://code.claude.com/docs/en/headless.md#stop-a-run-with-sigterm), [MCP work at exit](https://code.claude.com/docs/en/headless.md#mcp-work-when-a-non-interactive-run-ends).

## Codex — limited public-documentation research

The initial official CLI reference redirected to [developer commands](https://learn.chatgpt.com/docs/developer-commands?surface=cli). A legacy security URL redirected to a vulnerability-scanning page, not the desired CLI permission contract; that page is not evidence of either isolation or its absence.

A subsequent bounded public-documentation pass examined the [noninteractive guide](https://learn.chatgpt.com/docs/non-interactive-mode), [CLI reference](https://learn.chatgpt.com/docs/cli/reference), [configuration sample](https://learn.chatgpt.com/docs/config-file/config-sample) and [configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference). The material describes `ignore-user-config`, `ignore-rules`, ephemeral operation and individual shell/search/MCP/hooks/memory controls. That is not a verified combined all-tools-off, no-startup-side-effects contract preserving the existing subscription login. The CLI reference fetch covered the first100,000 of106,925 characters; configuration reference covered the first100,000 of156,740. Those fetches are partial coverage, not complete review of the referenced pages. Reconciled **2026-10-24T22:06:11Z** from the system clock; no client/authentication/inference was executed.

**No safe prompt-only invocation is selected from this research.** No installed Codex command or existing sign-in was inspected. A later bounded pass must resolve the exact versioned precedence and negative-isolation contracts, then prove instruction/tool/MCP/network/process behavior with disposable fixtures before enabling the adapter. No read-only sandbox shortcut, credential extraction or hidden fallback.

## Gemini CLI — bounded public-documentation pass

Reconciled **2026-10-24T22:21:08Z** from the system clock. The [headless guide](https://geminicli.com/docs/cli/headless/) describes `-p`/`--prompt` and JSON/JSONL output, including executed-tool events; headless is not tool-free. The legacy configuration link redirects to the [configuration reference](https://geminicli.com/docs/reference/configuration/), whose116,753 characters were fetched in two portions, including the final16,753 characters. This is documentation coverage, not installed-version or runtime qualification.

- The reference documents `-e none` for disabling extensions, `hooksConfig.enabled: false` for hooks and `admin.mcp.enabled: false` for MCP use. It does not establish that every startup initialization or managed-policy interaction is absent under the combined configuration.
- `tools.core` is a built-in allowlist, but the examined text does not explicitly define an empty array as removing every tool. `tools.allowed` and `--allowed-tools` bypass approval, not tool availability. Skills, agents and todo capability have separate settings; sandboxing or read-only approval is not a zero-tools guarantee.
- Context normally combines user, working-directory/ancestor and descendant `GEMINI.md` content, with imports. `context.memoryBoundaryMarkers: []` is documented to stop ancestor traversal, not all discovery. The examined text does not establish a no-context meaning for an empty `context.fileName` array. `context.includeDirectoryTree: false` removes only the initial tree contribution.
- Configuration precedence includes machine defaults/overrides, user/project files, environment and launch arguments. Empty-array/object merge behavior is not established. `GEMINI_CLI_HOME` relocates user storage but does not document preserving existing login in a fresh home. No credential extraction or policy bypass is selected.
- `general.sessionRetention.enabled: false` disables automatic cleanup, **not** session recording. `--delete-session` is not a no-save switch. Telemetry, usage statistics, update checks and generic local-environment loading have separate controls; the combined no-side-effects boundary remains unresolved.

**No prompt-only invocation is enabled from this pass.** No installed Gemini process, login store, native UI or inference was accessed. Verify exact-version behavior and synthetic negative/process-cleanup cases before offering an adapter; do not silently weaken the browser's input/privacy contract.

## Remaining clients and qualification

OpenCode invocation contracts are not researched by this pass. Ollama, LM Studio and llama.cpp remain explicit already-running loopback-server routes; the browser does not launch a discovered program or download a model. Loopback transport does not prove that the server runs inference offline.

Every eventual client needs exact executable identity/version, explicit user selection, supported model/auth scope, no shell interpolation, bounded stdin/stdout/stderr, fresh ephemeral sessions, no implicit data/tools, cancellation/process cleanup, visible errors and synthetic negative tests. Real provider behavior and native controls remain separate gates. No new execution permission, provider fallback, credential read or package completion follows from this document.
