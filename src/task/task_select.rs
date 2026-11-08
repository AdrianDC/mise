//! Task menu of `mise run --select` and `mise select`.

use std::collections::HashSet;
use std::sync::Arc;

use console::Term;
use demand::{DemandOption, MultiSelect, Theme};
use eyre::{Result, bail, ensure, eyre};
use itertools::Itertools;

use crate::config::Config;
use crate::task::{Task, TaskLoadContext};
use crate::ui::{ctrlc, style, theme};

/// A menu row: a task or a whole folder.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pick {
    Folder(String),
    Task(String),
}

/// A task as the menu lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    name: String,
    folder: String,
    label: String,
    description: String,
    checked: bool,
}

/// The name before the last `:`, prefixed with `global` for global tasks.
fn folder_of(task: &Task) -> String {
    let folder = task
        .name
        .rsplit_once(':')
        .map(|(folder, _)| folder)
        .unwrap_or_default();
    [if task.global { "global" } else { "" }, folder]
        .into_iter()
        .filter(|part| !part.is_empty())
        .join(":")
}

/// Whether a preset is this task's name, alias, or folder (`test` or `test:`),
/// or with `^`, the start of its name.
fn preset_matches(task: &Task, folder: &str, preset: &str) -> bool {
    if let Some(start) = preset.strip_prefix('^') {
        return task.name.starts_with(start) || task.display_name.starts_with(start);
    }
    preset == task.name
        || preset == task.display_name
        || (!folder.is_empty() && preset.strip_suffix(':').unwrap_or(preset) == folder)
        || task.aliases.iter().any(|alias| alias == preset)
}

/// Visible tasks, sorted by folder and name.
fn entries<'a>(
    tasks: impl IntoIterator<Item = &'a Task>,
    presets: &[String],
    aliases: bool,
) -> Vec<Entry> {
    tasks
        .into_iter()
        .filter(|task| !task.hide)
        .map(|task| {
            let folder = folder_of(task);
            let short = task
                .display_name
                .rsplit_once(':')
                .map_or(task.display_name.as_str(), |(_, short)| short);
            let label = if !aliases || task.aliases.is_empty() {
                short.to_string()
            } else {
                format!("{short} [{}]", task.aliases.join(", "))
            };
            Entry {
                checked: presets
                    .iter()
                    .any(|preset| preset_matches(task, &folder, preset)),
                name: task.name.clone(),
                label,
                // First line only, like `mise tasks ls`.
                description: task.description.lines().next().unwrap_or_default().into(),
                folder,
            }
        })
        .sorted_by(|a, b| (&a.folder, &a.label).cmp(&(&b.folder, &b.label)))
        .collect()
}

/// Minimum folder line width.
const FOLDER_LINE_WIDTH: usize = 40;

/// Tree branches before the tasks under a folder line.
const BRANCH: &str = "├─ ";
const LAST_BRANCH: &str = "└─ ";

/// A `title: ━━━` folder line, `width` columns wide.
fn folder_line(title: &str, width: usize) -> String {
    let rule = width.saturating_sub(title.chars().count() + 2).max(2);
    format!("{title}: {}", "━".repeat(rule))
}

/// Menu rows, with folder lines when there are several folders.
fn options(entries: &[Entry]) -> Vec<DemandOption<Pick>> {
    let grouped = entries.iter().map(|e| &e.folder).unique().count() > 1;
    let width = entries
        .iter()
        .map(|e| BRANCH.chars().count() + e.label.chars().count())
        .fold(FOLDER_LINE_WIDTH, usize::max);
    let mut options = vec![];
    let mut folder = None;
    for (i, entry) in entries.iter().enumerate() {
        if grouped && folder != Some(&entry.folder) {
            folder = Some(&entry.folder);
            let title = if entry.folder.is_empty() {
                "tasks"
            } else {
                &entry.folder
            };
            // Checked when its whole folder is, such as from a folder preset.
            let checked = entries
                .iter()
                .filter(|e| e.folder == entry.folder)
                .all(|e| e.checked);
            options.push(
                DemandOption::with_label(
                    folder_line(title, width),
                    Pick::Folder(entry.folder.clone()),
                )
                .selected(checked),
            );
        }
        let label = if grouped {
            let last = entries
                .get(i + 1)
                .is_none_or(|next| next.folder != entry.folder);
            let branch = if last { LAST_BRANCH } else { BRANCH };
            format!("{branch}{}", entry.label)
        } else {
            entry.label.clone()
        };
        let mut option =
            DemandOption::with_label(label, Pick::Task(entry.name.clone())).selected(entry.checked);
        if !entry.description.is_empty() {
            option = option.description(&entry.description);
        }
        options.push(option);
    }
    options
}

/// Picked task names, in menu order, without duplicates.
fn picked_names(entries: &[Entry], picks: &[Pick]) -> Vec<String> {
    let folders: HashSet<&str> = picks
        .iter()
        .filter_map(|pick| match pick {
            Pick::Folder(folder) => Some(folder.as_str()),
            Pick::Task(_) => None,
        })
        .collect();
    let tasks: HashSet<&str> = picks
        .iter()
        .filter_map(|pick| match pick {
            Pick::Task(name) => Some(name.as_str()),
            Pick::Folder(_) => None,
        })
        .collect();
    entries
        .iter()
        .filter(|e| folders.contains(e.folder.as_str()) || tasks.contains(e.name.as_str()))
        .map(|e| e.name.clone())
        .collect()
}

/// The theme, with circles as check marks.
fn menu_theme(mut theme: Theme) -> Theme {
    theme.selected_prefix = " ●".into();
    theme.unselected_prefix = " ○".into();
    theme
}

/// The preset tasks, in menu order; fails on unknown presets.
fn preset_names(tasks: &[&Task], entries: &[Entry], presets: &[String]) -> Result<Vec<String>> {
    ensure!(
        !presets.is_empty(),
        "--run requires task names, aliases, or folders"
    );
    if let Some(unknown) = presets.iter().find(|preset| {
        !tasks
            .iter()
            .any(|task| !task.hide && preset_matches(task, &folder_of(task), preset))
    }) {
        bail!("no task, alias, or folder named '{unknown}'");
    }
    Ok(entries
        .iter()
        .filter(|e| e.checked)
        .map(|e| e.name.clone())
        .collect())
}

/// Task names to run, in menu order: picked, or the presets if `run`.
pub(crate) fn pick<'a>(
    tasks: impl IntoIterator<Item = &'a Task>,
    presets: &[String],
    run: bool,
    aliases: bool,
) -> Result<Vec<String>> {
    if !run
        && (!console::user_attended_stderr()
            || !std::io::IsTerminal::is_terminal(&std::io::stdin()))
    {
        bail!("--select requires an interactive terminal");
    }
    let tasks = tasks.into_iter().collect_vec();
    let entries = entries(tasks.iter().copied(), presets, aliases);
    ensure!(
        !entries.is_empty(),
        "no tasks defined. see https://mise.jdx.dev/tasks/"
    );
    if run {
        return preset_names(&tasks, &entries, presets);
    }
    let theme = menu_theme(theme::get_theme());
    let menu = MultiSelect::new("Tasks")
        .description("Select tasks to run")
        .filterable(true)
        .options(options(&entries))
        .theme(&theme);
    let picks = menu.run().map_err(|err| {
        let _ = Term::stderr().show_cursor();
        eyre!(err)
    })?;
    Ok(picked_names(&entries, &picks))
}

/// The tasks to run, picked or given as presets.
pub async fn select_tasks(
    config: &Arc<Config>,
    presets: &[String],
    load_all: bool,
    only: bool,
    run: bool,
    aliases: bool,
) -> Result<Vec<Task>> {
    let ctx = load_all.then(TaskLoadContext::all);
    let tasks = config.tasks_with_context(ctx.as_ref()).await?;
    ctrlc::show_cursor_after_ctrl_c();
    pick(tasks.values(), presets, run, aliases)?
        .into_iter()
        .map(|name| {
            let mut task = tasks
                .get(&name)
                .cloned()
                .ok_or_else(|| eyre!("no tasks {} found", style::ered(&name)))?;
            if only {
                task.depends.clear();
                task.depends_post.clear();
                task.wait_for.clear();
            }
            Ok(task)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(name: &str, aliases: &[&str], global: bool) -> Task {
        Task {
            name: name.into(),
            display_name: name.into(),
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
            global,
            ..Default::default()
        }
    }

    fn presets(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn tasks_are_grouped_by_the_folder_in_their_name() {
        let tasks = [
            task("test:unit", &[], false),
            task("build", &[], false),
            task("test:e2e", &["e"], false),
            task("deploy", &[], true),
        ];
        let rows = |aliases| {
            entries(&tasks, &[], aliases)
                .into_iter()
                .map(|e| (e.folder, e.label))
                .collect_vec()
        };
        assert_eq!(
            rows(false).iter().map(|(_, label)| label).collect_vec(),
            ["build", "deploy", "e2e", "unit"]
        );
        assert_eq!(
            rows(true),
            [
                ("".into(), "build".into()),
                ("global".into(), "deploy".into()),
                ("test".into(), "e2e [e]".into()),
                ("test".into(), "unit".into()),
            ]
        );
    }

    #[test]
    fn hidden_tasks_are_not_listed() {
        let mut hidden = task("secret", &[], false);
        hidden.hide = true;
        let tasks = [task("build", &[], false), hidden];
        assert_eq!(entries(&tasks, &[], false).len(), 1);
    }

    #[test]
    fn presets_check_tasks_by_name_alias_or_folder() {
        let tasks = [
            task("build", &["b"], false),
            task("lint", &[], false),
            task("test:unit", &[], false),
            task("test:e2e", &[], false),
        ];
        let checked = |names: &[&str]| {
            entries(&tasks, &presets(names), false)
                .into_iter()
                .filter(|e| e.checked)
                .map(|e| e.name)
                .collect_vec()
        };
        assert_eq!(checked(&["lint"]), ["lint"]);
        assert_eq!(checked(&["b"]), ["build"]);
        assert_eq!(checked(&["test"]), ["test:e2e", "test:unit"]);
        assert_eq!(checked(&["test:"]), ["test:e2e", "test:unit"]);
        assert_eq!(checked(&["^test:"]), ["test:e2e", "test:unit"]);
        assert_eq!(checked(&["^test:u", "^l"]), ["lint", "test:unit"]);
        assert!(checked(&["nope"]).is_empty());
    }

    #[test]
    fn a_folder_preset_checks_its_folder_line() {
        let tasks = [
            task("build", &[], false),
            task("test:unit", &[], false),
            task("test:e2e", &[], false),
        ];
        let checked = options(&entries(&tasks, &presets(&["test:"]), false))
            .into_iter()
            .filter(|o| o.selected)
            .map(|o| o.item)
            .collect_vec();
        assert_eq!(
            checked,
            [
                Pick::Folder("test".into()),
                Pick::Task("test:e2e".into()),
                Pick::Task("test:unit".into()),
            ]
        );
    }

    #[test]
    fn folder_lines_appear_only_with_several_folders() {
        let one = entries(&[task("a", &[], false), task("b", &[], false)], &[], false);
        assert!(
            options(&one)
                .iter()
                .all(|o| matches!(o.item, Pick::Task(_)))
        );

        let several = entries(
            &[
                task("a", &[], false),
                task("test:b", &[], false),
                task("test:c", &[], false),
            ],
            &[],
            false,
        );
        let labels = options(&several).into_iter().map(|o| o.label).collect_vec();
        assert_eq!(
            labels,
            [
                folder_line("tasks", 40),
                "└─ a".into(),
                folder_line("test", 40),
                "├─ b".into(),
                "└─ c".into()
            ]
        );
    }

    #[test]
    fn run_takes_the_presets_without_a_menu() {
        let tasks = [
            task("build", &["b"], false),
            task("test:unit", &[], false),
            task("test:e2e", &[], false),
        ];
        let run = |presets: &[&str]| {
            let presets = presets.iter().map(|p| p.to_string()).collect_vec();
            let refs = tasks.iter().collect_vec();
            preset_names(&refs, &entries(&tasks, &presets, false), &presets)
        };
        assert_eq!(
            run(&["test", "b"]).unwrap(),
            ["build", "test:e2e", "test:unit"]
        );
        assert!(run(&[]).is_err());
        assert!(run(&["b", "nope"]).is_err());
    }

    #[test]
    fn check_marks_are_circles() {
        let theme = menu_theme(Theme::new());
        assert_eq!(theme.selected_prefix, " ●");
        assert_eq!(theme.unselected_prefix, " ○");
    }

    #[test]
    fn folder_lines_span_the_widest_label() {
        assert_eq!(folder_line("test", 12), "test: ━━━━━━");
        assert_eq!(folder_line("a-long-folder", 8), "a-long-folder: ━━");
        assert_eq!(folder_line("tasks", 40).chars().count(), 40);
    }

    #[test]
    fn a_checked_folder_runs_all_its_tasks_once_in_menu_order() {
        let tasks = [
            task("build", &[], false),
            task("test:unit", &[], false),
            task("test:e2e", &[], false),
        ];
        let entries = entries(&tasks, &[], false);
        let picks = [
            Pick::Task("test:unit".into()),
            Pick::Folder("test".into()),
            Pick::Task("build".into()),
        ];
        assert_eq!(
            picked_names(&entries, &picks),
            ["build", "test:e2e", "test:unit"]
        );
    }
}
