# Keyboard shortcuts

The default shortcuts follow JetBrains IDEs, so DataGrip and IntelliJ users can keep their habits. Every command, with or without a shortcut, can be run from **Find Action…**.

## Defaults

"Context" is where the shortcut works: anywhere, in a console editor, or in a result grid.

### Navigation and application

| Command | Windows / Linux | macOS | Context |
|---|---|---|---|
| **Search Everywhere** | Shift, Shift | Shift, Shift | Anywhere |
| **Find Action…** | Ctrl+Shift+A | Cmd+Shift+A | Anywhere |
| **Settings…** | Ctrl+Alt+S | Cmd+, | Anywhere |
| **Quit** | — | Cmd+Q | Anywhere |
| **Toggle Database Explorer** | Alt+1 | Cmd+1 | Anywhere |
| **Toggle Bottom Panel** | Alt+4 | Cmd+4 | Anywhere |
| **Refresh** (explorer) | Ctrl+F5 | Cmd+F5 | Anywhere |
| **New Console** | Ctrl+Shift+L | Cmd+Shift+L | Main window |

### Console

| Command | Windows / Linux | macOS |
|---|---|---|
| **Execute** (statement under cursor or selection) | Ctrl+Enter | Cmd+Enter |
| **Execute Script** | Ctrl+Shift+Enter | Cmd+Shift+Enter |
| **Stop Query** | Ctrl+F2 | Cmd+F2 |
| **Explain Plan** | Ctrl+Shift+E | Cmd+Shift+E |
| **Explain Analyze** | Ctrl+Alt+Shift+E | Cmd+Alt+Shift+E |
| **Save File** | Ctrl+S | Cmd+S |
| Completion | Ctrl+Space | Ctrl+Space |
| **Reformat Code** | Ctrl+Alt+L | Cmd+Alt+L |
| **Go to Declaration** | Ctrl+B | Cmd+B |
| **Quick Documentation** | Ctrl+Q | F1 |
| **Rename…** (not active yet) | Shift+F6 | Shift+F6 |
| **Insert Live Template…** (not active yet) | Ctrl+J | Cmd+J |

### Result grid

| Command | Windows / Linux | macOS |
|---|---|---|
| **Copy** | Ctrl+C | Cmd+C |
| **Find in Results** | Ctrl+F | Cmd+F |
| **Submit Changes** | Ctrl+Enter | Cmd+Enter |
| **Revert Changes** | Ctrl+Alt+Z | Cmd+Alt+Z |

### Dialogs

- In the consent dialog for agents, Esc means **Deny**.
- In the AI Assistant, Enter sends and Shift+Enter adds a line.

Commands without a default shortcut include **Toggle Right Panel**, **Reset Layout**, **New Data Source…**, **Driver Manager…**, **Export Data…**, **Import Data…**, **Query History**, **Local History**, **Project Files**, **ER Diagram**, **Compare Schemas**, **New Table…**, **AI Assistant**, **Ask AI…** and the other AI commands.

## Changing shortcuts

Put a `keymap.toml` file in the configuration directory (see [Settings and files](settings-and-files.md)). Each `[[binding]]` adds a shortcut or replaces the default with the same keys and context:

```toml
# Also run the whole script with Ctrl+Alt+Enter
[[binding]]
keys = "ctrl-alt-enter"
action = "ferrodb::ExecuteScript"
context = "Console"

# Ask the AI assistant with Ctrl+Alt+I
[[binding]]
keys = "secondary-alt-i"
action = "ferrodb::AskAi"
context = "Console"

# Remove the default Ctrl+Alt+S binding (Windows and Linux)
[[binding]]
keys = "ctrl-alt-s"
action = ""
```

| Field | Meaning |
|---|---|
| `keys` | Keys joined with `-`, e.g. `ctrl-shift-a`, `alt-1`, `f5`. `secondary-` means Cmd on macOS and Ctrl elsewhere. Several keystrokes in a sequence are separated by spaces |
| `action` | The command's name, `ferrodb::<Name>` (see the table below). An empty string removes the binding of these keys |
| `context` | Optional: `Console`, `ResultGrid` or `Workspace`. Without it, the binding works everywhere |

The keymap is read at startup: restart FerroDb after changing it. Problems (an unknown action, a key that cannot be parsed) are reported in the **Output** panel, and the rest of the file still applies. Search Everywhere's double Shift cannot be changed.

### Action names

| Action | Command |
|---|---|
| `ferrodb::FindAction` | Find Action… |
| `ferrodb::SearchEverywhere` | Search Everywhere |
| `ferrodb::OpenSettings` | Settings… |
| `ferrodb::ToggleTheme` | Toggle Light/Dark Theme |
| `ferrodb::OpenProject`, `ferrodb::RecentProjects` | Open Project…, Recent Projects… |
| `ferrodb::About`, `ferrodb::Quit` | About FerroDb, Quit |
| `ferrodb::ToggleExplorer`, `ferrodb::ToggleBottomPanel`, `ferrodb::ToggleRightPanel`, `ferrodb::ResetLayout` | Layout |
| `ferrodb::NewDataSource`, `ferrodb::EditDataSource`, `ferrodb::OpenDriverManager`, `ferrodb::RefreshExplorer` | Data sources |
| `ferrodb::NewConsole` | New Console |
| `ferrodb::ExecuteStatement`, `ferrodb::ExecuteScript`, `ferrodb::StopQuery` | Execute, Execute Script, Stop Query |
| `ferrodb::ExplainPlan`, `ferrodb::ExplainAnalyze` | Explain Plan, Explain Analyze |
| `ferrodb::SaveFile`, `ferrodb::FormatCode` | Save File, Reformat Code |
| `ferrodb::Rename`, `ferrodb::GoToDeclaration`, `ferrodb::LiveTemplates`, `ferrodb::QuickDocumentation` | Editor navigation |
| `ferrodb::CopySelection`, `ferrodb::FindInResults`, `ferrodb::SubmitChanges`, `ferrodb::RevertChanges`, `ferrodb::LoadMore` | Result grid |
| `ferrodb::ExportData`, `ferrodb::ImportData` | Export Data…, Import Data… |
| `ferrodb::ShowQueryHistory`, `ferrodb::ShowLocalHistory`, `ferrodb::ShowProjectFiles` | Query History, Local History, Project Files |
| `ferrodb::NewTable`, `ferrodb::ShowErDiagram`, `ferrodb::CompareSchemas` | New Table…, ER Diagram, Compare Schemas |
| `ferrodb::ToggleAssistant`, `ferrodb::AskAi`, `ferrodb::AiExplainQuery`, `ferrodb::AiOptimizeQuery`, `ferrodb::AiFixQuery` | AI Assistant and AI commands |
| `ferrodb::OpenAiSettings`, `ferrodb::OpenAgentsSettings` | AI Assistant Settings…, Agents and MCP Settings… |
| `ferrodb::OpenScriptsFolder`, `ferrodb::NewExtractorScript`, `ferrodb::NewAggregatorScript`, `ferrodb::ReloadScripts` | Extension scripts |
