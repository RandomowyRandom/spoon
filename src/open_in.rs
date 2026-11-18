//! "Open in": the terminals, file manager and editors found on this machine, and
//! the repository's page on its remote host. Detection follows SourceGit: known
//! install folders, the search path, vswhere, and the JetBrains Toolbox state.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Terminal,
    Files,
    Editor,
}

/// An installed app that can open a folder.
#[derive(Clone, Debug)]
pub struct Target {
    pub name: String,
    pub kind: Kind,
    exe: PathBuf,
    /// `{dir}` in an argument stands for the folder. Every app also starts in it.
    args: Vec<String>,
}

/// Editors that take the folder as their only argument: name, path under
/// `Programs` or `Program Files` on Windows, command names elsewhere.
const EDITORS: [(&str, &str, &[&str]); 7] = [
    ("Visual Studio Code", r"Microsoft VS Code\Code.exe", &["code"]),
    ("VS Code Insiders", r"Microsoft VS Code Insiders\Code - Insiders.exe", &["code-insiders"]),
    ("VSCodium", r"VSCodium\VSCodium.exe", &["codium"]),
    ("Cursor", r"cursor\Cursor.exe", &["cursor"]),
    ("Windsurf", r"Windsurf\Windsurf.exe", &["windsurf"]),
    ("Zed", r"Zed\Zed.exe", &["zeditor", "zed"]),
    ("Sublime Text", r"Sublime Text\sublime_text.exe", &["subl"]),
];

/// Programs made for comparing files: name, paths under `Programs` or `Program Files`
/// on Windows, command names elsewhere, and the arguments that go before the two files.
const DIFF_TOOLS: [(&str, &[&str], &[&str], &[&str]); 5] = [
    ("Beyond Compare", &[r"Beyond Compare 5\BComp.exe", r"Beyond Compare 4\BComp.exe"], &["bcompare"], &[]),
    ("WinMerge", &[r"WinMerge\WinMergeU.exe"], &[], &["-u", "-e"]),
    ("KDiff3", &[r"KDiff3\bin\kdiff3.exe", r"KDiff3\kdiff3.exe"], &["kdiff3"], &[]),
    ("Meld", &[r"Meld\Meld.exe"], &["meld"], &[]),
    ("P4Merge", &[r"Perforce\p4merge.exe"], &["p4merge"], &[]),
];

/// A program that shows two versions of a file side by side.
#[derive(Clone, Debug, PartialEq)]
pub struct DiffTool {
    pub name: String,
    exe: PathBuf,
    args: Vec<String>,
}

impl DiffTool {
    pub(crate) fn new(name: &str, exe: PathBuf, args: &[&str]) -> Self {
        Self { name: name.to_string(), exe, args: args.iter().map(|a| a.to_string()).collect() }
    }

    /// Where the program is installed, to tell two installs of it apart.
    pub fn location(&self) -> String {
        self.exe.display().to_string()
    }

    /// The command line for `git difftool`, which adds the old and the new file at its
    /// end. Git hands it to a POSIX shell, so the path takes forward slashes and single
    /// quotes.
    pub fn command(&self) -> String {
        let exe = self.exe.display().to_string().replace('\\', "/").replace('\'', r"'\''");
        std::iter::once(format!("'{exe}'")).chain(self.args.iter().cloned()).collect::<Vec<_>>().join(" ")
    }
}

/// The arguments that make an editor compare two files, if it can.
pub(crate) fn editor_diff_args(name: &str) -> Option<&'static [&'static str]> {
    const CODE_FAMILY: [&str; 5] = ["Visual Studio Code", "VS Code Insiders", "VSCodium", "Cursor", "Windsurf"];
    if CODE_FAMILY.contains(&name) {
        // --wait: git removes its temporary files as soon as the program returns.
        Some(&["--wait", "--diff"])
    } else if EDITORS.iter().any(|(editor, ..)| *editor == name) {
        None
    } else if name.starts_with("Visual Studio") {
        // Git's shell on Windows rewrites an argument that starts with one slash into
        // a path; two slashes come out as the single one devenv expects.
        Some(&["//diff"])
    } else {
        Some(&["diff"]) // JetBrains IDEs
    }
}

/// The diff programs on this machine: the dedicated ones first, then the editors that
/// can compare files. Found once, like [`targets`].
pub fn diff_tools() -> &'static [DiffTool] {
    static FOUND: OnceLock<Vec<DiffTool>> = OnceLock::new();
    FOUND.get_or_init(|| {
        let roots = [env_dir("LOCALAPPDATA").map(|d| d.join("Programs")), env_dir("ProgramFiles"), env_dir("ProgramFiles(x86)")];
        let installed = |rels: &[&str]| roots.iter().flatten().flat_map(|root| rels.iter().map(move |rel| root.join(rel))).find(|p| p.is_file());
        let dedicated = DIFF_TOOLS.iter().filter_map(|(name, rels, commands, args)| {
            let exe = if cfg!(windows) { installed(rels) } else { on_path(commands) };
            Some(DiffTool::new(name, exe?, args))
        });
        let editors = targets().iter().filter(|target| target.kind == Kind::Editor).filter_map(|target| Some(DiffTool::new(&target.name, target.exe.clone(), editor_diff_args(&target.name)?)));
        dedicated.chain(editors).collect()
    })
}

/// Linux terminals, most generic first, with the arguments that set the folder
/// for those that do not simply start in the current one.
const TERMINALS: [(&str, &[&str]); 14] = [
    ("x-terminal-emulator", &[]),
    ("gnome-terminal", &[]),
    ("konsole", &[]),
    ("ptyxis", &["--new-window", "--working-directory={dir}"]),
    ("xfce4-terminal", &[]),
    ("mate-terminal", &[]),
    ("lxterminal", &[]),
    ("deepin-terminal", &[]),
    ("ghostty", &[]),
    ("kitty", &[]),
    ("alacritty", &[]),
    ("wezterm", &["start", "--cwd", "{dir}"]),
    ("foot", &[]),
    ("xterm", &[]),
];

impl Target {
    fn new(name: &str, kind: Kind, exe: PathBuf, args: &[&str]) -> Self {
        Self { name: name.to_string(), kind, exe, args: args.iter().map(|a| a.to_string()).collect() }
    }

    /// Starts the app on `dir` without waiting for it.
    pub fn open(&self, dir: &Path) -> Result<(), String> {
        let dir = native(dir);
        let mut command = Command::new(&self.exe);
        command.args(self.args.iter().map(|a| a.replace("{dir}", &dir))).current_dir(&dir);
        #[cfg(windows)]
        if self.kind == Kind::Terminal {
            // A shell started by a windowed app needs a console of its own.
            std::os::windows::process::CommandExt::creation_flags(&mut command, 0x0000_0010); // CREATE_NEW_CONSOLE
        }
        let mut child = command.spawn().map_err(|e| format!("Cannot start {}: {e}", self.name))?;
        // Reap it when it ends; its exit code says nothing about opening the folder.
        std::thread::spawn(move || drop(child.wait()));
        Ok(())
    }
}

/// The folder as its platform writes it. Git reports `C:/Users/me/repo`, which
/// Explorer does not take for a path.
fn native(dir: &Path) -> String {
    let text = dir.display().to_string();
    if cfg!(windows) { text.replace('/', "\\") } else { text }
}

/// What this machine can open a folder in: terminals, the file manager, then editors.
/// Found once; the first call can take a moment, so warm it up off the UI thread.
pub fn targets() -> &'static [Target] {
    static FOUND: OnceLock<Vec<Target>> = OnceLock::new();
    FOUND.get_or_init(detect)
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// The first of `names` found on the search path.
fn on_path(names: &[&str]) -> Option<PathBuf> {
    let dirs: Vec<PathBuf> = std::env::split_paths(&std::env::var_os("PATH")?).collect();
    // symlink_metadata: Windows app aliases such as wt.exe are links `is_file` cannot follow.
    names.iter().flat_map(|name| dirs.iter().map(move |dir| dir.join(name))).find(|path| path.symlink_metadata().is_ok_and(|m| !m.is_dir()))
}

fn detect() -> Vec<Target> {
    let mut found = Vec::new();
    let mut add = |name: &str, kind: Kind, exe: Option<PathBuf>, args: &[&str]| found.extend(exe.map(|exe| Target::new(name, kind, exe, args)));
    let toolbox;
    if cfg!(windows) {
        let local = env_dir("LOCALAPPDATA");
        let roots = [local.as_ref().map(|d| d.join("Programs")), env_dir("ProgramFiles")];
        // Windows Terminal when installed, else the PowerShell that always is.
        match on_path(&["wt.exe"]) {
            Some(wt) => add("Terminal", Kind::Terminal, Some(wt), &["-d", "{dir}"]),
            None => add("Terminal", Kind::Terminal, on_path(&["pwsh.exe", "powershell.exe"]), &[]),
        }
        // git-bash.exe sits at the top of the Git folder, above cmd\git.exe.
        let git_bash = on_path(&["git.exe"]).and_then(|git| git.ancestors().skip(1).take(3).map(|dir| dir.join("git-bash.exe")).find(|p| p.is_file()));
        add("Git Bash", Kind::Terminal, git_bash, &["--cd={dir}"]);
        add("File Explorer", Kind::Files, on_path(&["explorer.exe"]), &["{dir}"]);
        for (name, rel, _) in EDITORS {
            add(name, Kind::Editor, roots.iter().flatten().map(|root| root.join(rel)).find(|p| p.is_file()), &["{dir}"]);
        }
        for (name, exe) in visual_studio() {
            add(&name, Kind::Editor, Some(exe), &["{dir}"]);
        }
        toolbox = local.map(|d| d.join("JetBrains").join("Toolbox"));
    } else {
        let terminal = TERMINALS.iter().find_map(|(name, args)| Some((on_path(&[*name])?, *args)));
        if let Some((exe, args)) = terminal {
            add("Terminal", Kind::Terminal, Some(exe), args);
        }
        add("File manager", Kind::Files, on_path(&["xdg-open"]), &["{dir}"]);
        for (name, _, commands) in EDITORS {
            add(name, Kind::Editor, on_path(commands), &["{dir}"]);
        }
        let data = env_dir("XDG_DATA_HOME").or_else(|| Some(env_dir("HOME")?.join(".local/share")));
        toolbox = data.map(|d| d.join("JetBrains/Toolbox"));
    }
    let state = toolbox.and_then(|dir| std::fs::read_to_string(dir.join("state.json")).ok()).unwrap_or_default();
    for (name, exe) in jetbrains(&state) {
        add(&name, Kind::Editor, Some(exe).filter(|p| p.is_file()), &["{dir}"]);
    }
    found
}

/// Visual Studio installs, as vswhere lists them: display name and `devenv.exe`.
fn visual_studio() -> Vec<(String, PathBuf)> {
    let vswhere = env_dir("ProgramFiles(x86)").map(|d| d.join(r"Microsoft Visual Studio\Installer\vswhere.exe")).filter(|p| p.is_file());
    let Some(vswhere) = vswhere else { return Vec::new() };
    let mut command = Command::new(vswhere);
    command.args(["-prerelease", "-utf8", "-format", "value", "-property", "productPath"]);
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut command, 0x0800_0000); // CREATE_NO_WINDOW
    let Ok(out) = command.output() else { return Vec::new() };
    let installs = String::from_utf8_lossy(&out.stdout).lines().map(|line| PathBuf::from(line.trim())).filter(|p| p.is_file()).collect::<Vec<_>>();
    installs.into_iter().map(|exe| (visual_studio_name(&exe), exe)).collect()
}

/// `...\Microsoft Visual Studio\2022\Community\...\devenv.exe` -> `Visual Studio 2022`.
pub(crate) fn visual_studio_name(exe: &Path) -> String {
    // Split by hand: a Windows path has no components for `Path` on other systems.
    let text = exe.to_string_lossy();
    let year = text.split(['\\', '/']).find(|part| part.len() == 4 && part.bytes().all(|b| b.is_ascii_digit()));
    year.map_or_else(|| "Visual Studio".to_string(), |year| format!("Visual Studio {year}"))
}

/// The IDEs in a JetBrains Toolbox `state.json`: display name and launcher path.
pub(crate) fn jetbrains(state: &str) -> Vec<(String, PathBuf)> {
    let parsed: serde_json::Value = serde_json::from_str(state).unwrap_or_default();
    let tools = parsed["tools"].as_array().cloned().unwrap_or_default();
    tools
        .iter()
        .filter_map(|tool| {
            let text = |key: &str| tool[key].as_str().filter(|value| !value.is_empty());
            Some((text("displayName")?.to_string(), Path::new(text("installLocation")?).join(text("launchCommand")?)))
        })
        .collect()
}

/// The web page of a remote. `git@github.com:me/repo.git`, `ssh://git@host:22/me/repo`
/// and `https://user@host/me/repo.git` all give `https://host/me/repo`. `None` for a
/// local path, which has no page.
pub fn web_url(remote: &str) -> Option<String> {
    let url = remote.trim().trim_end_matches('/').trim_end_matches(".git");
    let (scheme, host, path) = match url.split_once("://") {
        Some((scheme, rest)) if ["http", "https", "ssh", "git"].contains(&scheme) => {
            let (authority, path) = rest.split_once('/')?;
            let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
            // An ssh or git port is not the web port.
            match scheme {
                "http" | "https" => (scheme, host, path),
                _ => ("https", host.split(':').next()?, path),
            }
        }
        Some(_) => return None,
        None => {
            // scp form, `[user@]host:path`. A one-letter "host" is a Windows drive.
            let (left, path) = url.split_once(':')?;
            let host = left.rsplit_once('@').map_or(left, |(_, host)| host);
            if host.len() < 2 || host.contains(['/', '\\']) {
                return None;
            }
            ("https", host, path)
        }
    };
    let path = path.trim_start_matches('/');
    (!host.is_empty() && !path.is_empty()).then(|| format!("{scheme}://{host}/{path}"))
}

/// What to call the host of a web address in a menu: `GitHub`, or the host itself.
pub fn site_name(web: &str) -> &str {
    let host = web.split_once("://").map_or(web, |(_, rest)| rest).split('/').next().unwrap_or(web);
    match host {
        "github.com" => "GitHub",
        "gitlab.com" => "GitLab",
        "bitbucket.org" => "Bitbucket",
        _ => host,
    }
}
