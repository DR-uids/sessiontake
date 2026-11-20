export type ProjectSource = {
  project: string | null;
  projectExists: boolean;
  worktree: string | null;
  repository?: string | null;
  worktreeRepository?: string | null;
};

export type ProjectGroup = {
  /** Stable filter value; never shown to the user. */
  key: string;
  /** Display name, including a parent qualifier for duplicate ordinary paths. */
  name: string;
  /** Workspace paths that belong to this project. */
  paths: string[];
  /** Worktree directory names of this repository, sorted. */
  worktrees: string[];
};

export type ProjectIndex = {
  groups: ProjectGroup[];
  pathLabels: Map<string, string>;
  pathGroups: Map<string, string>;
};

type PlainPath = {
  path: string;
  name: string;
  parents: string[];
  merged: boolean;
};

export function baseName(path: string): string {
  return path.split("/").filter(Boolean).pop() ?? "/";
}

export function codexWorkspaceTag(path: string | null): "Codex" | "ChatGPT" {
  return path?.split("/").some((part) => part.toLowerCase() === "chatgpt") ? "ChatGPT" : "Codex";
}

function suffix(parents: string[], depth: number): string {
  return depth > parents.length
    ? "/" + parents.join("/")
    : parents.slice(-depth).join("/");
}

// Vibe Kanban starts one git worktree per task. Its sessions all carry a
// different cwd, but they are one repository; only the worktree directory name
// (for example "2222-header") tells the tasks apart.
export function groupProjects(rows: Iterable<ProjectSource>): ProjectIndex {
  const repositories = new Map<
    string,
    { name: string; paths: Set<string>; worktrees: Set<string> }
  >();
  const pending: ProjectSource[] = [];
  const plains: PlainPath[] = [];
  const seen = new Set<string>();
  function record(key: string, name: string, path: string, worktree: string | null) {
    const group = repositories.get(key) ?? { name, paths: new Set(), worktrees: new Set() };
    group.paths.add(path);
    if (worktree) group.worktrees.add(worktree);
    repositories.set(key, group);
  }
  for (const row of rows) {
    const path = row.project;
    if (!path || seen.has(path)) continue;
    seen.add(path);
    const segments = path.split("/").filter(Boolean);
    const name = segments.pop() ?? "/";
    if (row.repository) {
      record(`git:${row.repository}`, baseName(row.repository), path, row.worktree);
    } else if (row.worktree) {
      pending.push(row);
    } else {
      plains.push({ path, name, parents: segments, merged: false });
    }
  }

  const plainsByName = new Map<string, PlainPath[]>();
  for (const plain of plains) {
    const list = plainsByName.get(plain.name) ?? [];
    list.push(plain);
    plainsByName.set(plain.name, list);
  }
  const knownRepositories = new Map<string, string[]>();
  for (const [key, group] of repositories) {
    const matches = knownRepositories.get(group.name) ?? [];
    matches.push(key);
    knownRepositories.set(group.name, matches);
  }
  for (const row of pending) {
    const path = row.project!;
    const name = row.worktreeRepository ?? baseName(path);
    const known = knownRepositories.get(name) ?? [];
    const ordinary = plainsByName.get(name) ?? [];
    const matching = known.length === 1 && ordinary.length === 0 ? known[0] : null;
    const fallback = !known.length && ordinary.length < 2
      ? `repo:${name}`
      : `worktree:${path}`;
    record(matching ?? fallback, name, path, row.worktree);
    if (!known.length && ordinary.length === 1) {
      record(fallback, name, ordinary[0].path, null);
      ordinary[0].merged = true;
    }
  }

  const groups: ProjectGroup[] = [];
  const pathLabels = new Map<string, string>();
  const pathGroups = new Map<string, string>();
  for (const [key, group] of repositories) {
    const name = group.name;
    const paths = [...group.paths].sort();
    const worktrees = [...group.worktrees].sort((a, b) => a.localeCompare(b));
    groups.push({ key, name, paths, worktrees });
    for (const path of paths) {
      pathLabels.set(path, name);
      pathGroups.set(path, key);
    }
  }

  const unmerged = plains.filter((plain) => !plain.merged);
  const unmergedByName = new Map<string, PlainPath[]>();
  for (const plain of unmerged) {
    const list = unmergedByName.get(plain.name) ?? [];
    list.push(plain);
    unmergedByName.set(plain.name, list);
  }
  for (const [name, list] of unmergedByName) {
    for (const entry of list) {
      let label = name;
      if (list.length > 1) {
        // Use the shortest parent suffix that identifies this workspace.
        let qualifier = entry.path;
        for (let depth = 1; depth <= entry.parents.length + 1; depth++) {
          const candidate = suffix(entry.parents, depth);
          if (
            list.every(
              (other) =>
                other === entry ||
                suffix(other.parents, depth) !== candidate,
            )
          ) {
            qualifier = candidate;
            break;
          }
        }
        label += " · " + qualifier;
      }
      const key = `path:${entry.path}`;
      groups.push({ key, name: label, paths: [entry.path], worktrees: [] });
      pathLabels.set(entry.path, label);
      pathGroups.set(entry.path, key);
    }
  }
  const byLabel = new Map<string, ProjectGroup[]>();
  for (const group of groups) {
    const sameLabel = byLabel.get(group.name) ?? [];
    sameLabel.push(group);
    byLabel.set(group.name, sameLabel);
  }
  for (const sameLabel of byLabel.values()) {
    if (sameLabel.length < 2) continue;
    for (const group of sameLabel) {
      const path = group.key.startsWith("git:") ? group.key.slice(4) : group.paths[0];
      group.name += ` · ${path.slice(0, path.lastIndexOf("/")) || "/"}`;
      for (const path of group.paths) pathLabels.set(path, group.name);
    }
  }
  return { groups, pathLabels, pathGroups };
}
