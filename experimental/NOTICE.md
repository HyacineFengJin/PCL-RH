# Shared components and adaptations

The launcher's own extension host, Agent adapters and integration code follow the project's [MIT License](../LICENSE). Directly incorporated modules retain their own notices:

| Component | Attribution and license |
| --- | --- |
| `runtime/vendor/maker/` | Mod Creator Lab contributors, MIT; [original notice](runtime/vendor/maker/LICENSE) |
| `runtime/vendor/porter/` | Mod Port Lab contributors, MIT; [original notice](runtime/vendor/porter/LICENSE) |
| Pi SDK | Fixed npm dependency; original package license and upstream copyright, not PCL RH copyright |
| Cordis | Used by RH Code through `@deepseek-ai/cordis`; MIT, Shigma; [notice](../LICENSES/CORDIS.txt) |

[SOURCES.json](SOURCES.json) records the current adapted module versions and included vendor fingerprints. The original module's license and attribution remain in force for adapted code. Package versions do not imply that all current files are unchanged upstream copies.

The project borrows architecture and workflow ideas from DeepSeek Harness; it does not bundle the complete DSH Agent. Standalone lab servers, demonstration archives, user projects and local test records are outside this integration.

See [copyright scope](../COPYRIGHT.md) and [third-party dependencies](../THIRD_PARTY_NOTICES.md) for source, package and distribution boundaries.
