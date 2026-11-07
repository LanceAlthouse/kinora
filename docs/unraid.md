# Kinora on Unraid

## Install

### From Community Applications

Once Kinora is listed: **Apps** tab → search **Kinora** → **Install**.

### By hand (before it is listed, or to test a template change)

1. Copy [`packaging/unraid/kinora.xml`](../packaging/unraid/kinora.xml) to
   `/boot/config/plugins/dockerMan/templates-user/my-kinora.xml`
   (for example with the **Config** share over SMB, or `wget` in the Unraid terminal).
2. **Docker** tab → **Add Container** → pick **kinora** under *Template*.
3. Check the paths (below), then **Apply**.

### Paths and settings

| Field | Container path | Default | Notes |
| --- | --- | --- | --- |
| Config | `/config` | `/mnt/cache/appdata/kinora` | SQLite database. Keep it on a pool (SSD). Through `/mnt/user` (the FUSE share layer) queries took 2-3 s. Change `cache` if your pool has another name. |
| Media | `/media` | `/mnt/user/media` | Read-only. `/mnt/user` is fine here. |
| Transcode temp | `/transcode` | `/mnt/cache/transcode/kinora` | SSD pool. `/tmp/kinora` (RAM) works with plenty of free memory. |
| Intel/AMD GPU | `/dev/dri` | empty | Enter `/dev/dri` for Quick Sync or AMD. The container won't start if the device doesn't exist. |
| TMDB API key | `TMDB_API_KEY` | empty | Optional; also settable in Admin → Modules. |

**NVIDIA:** install the **Nvidia Driver** plugin (Apps tab), reboot if it asks,
then in the Kinora template switch on **Advanced View** and add
`--runtime=nvidia` to **Extra Parameters**. Leave
`NVIDIA_VISIBLE_DEVICES=all` (or set one GPU's UUID) and
`NVIDIA_DRIVER_CAPABILITIES=all`; the `graphics` capability is needed for
Vulkan, which HDR and Dolby Vision tone mapping use.

`--runtime=nvidia` is not in the template by default, because the container
fails to start on servers without the NVIDIA runtime.

## Publishing to Community Applications (maintainers)

Community Applications (CA) reads templates from a **public GitHub repository
with a `ca_profile.xml` at its root**. The templates live in a separate repo,
[`LanceAlthouse/unraid-templates`](https://github.com/LanceAlthouse/unraid-templates),
so the CA scanner doesn't see this repo's Android XML files, and so a CA
policy problem can never affect the main repo.

Layout of that repo:

```
unraid-templates/
├── LICENSE              # an OSI license is required (MIT is fine for templates)
├── README.md
├── ca_profile.xml       # who you are; <Profile> must not be empty
└── templates/
    └── kinora.xml       # identical copy of packaging/unraid/kinora.xml
```

Requirements CA checks:

- The template repo is public, not archived, has an OSI license and a
  `ca_profile.xml` with a non-empty `<Profile>`.
- The image in `<Repository>` can be pulled without logging in.
  `ghcr.io/lancealthouse/kinora` is public (anonymous pulls work).
- Your GitHub account has two-factor authentication on.
- Descriptions are plain text: no HTML, no `<like_this>` placeholders.
  `[br]` makes a line break in `<Overview>`.

Submitting:

1. Go to <https://ca.unraid.net/submit/new> and sign in with your Unraid
   account.
2. Enter `https://github.com/LanceAlthouse/unraid-templates`.
3. Run **Validate**, then **Scan**. Fix anything reported, push, and repeat.
4. **Submit**. Approved apps appear after the next CA catalog build.

A support thread on the Unraid forum is optional. `<Support>` points to
GitHub issues. If you open one later (Docker Containers subforum), put its
URL in `<Forum>` in `ca_profile.xml` and in `<Support>` in the template.

### Updating

- **New Kinora version**: nothing to do. Unraid pulls `:latest`, and users see
  "update ready" on the Docker tab.
- **Template change** (new variable, new path): change
  `packaging/unraid/kinora.xml` here, copy it to `templates/kinora.xml` in
  `unraid-templates`, and push. CA picks it up within a few hours. Existing
  installs keep their own copy; Unraid doesn't overwrite users' settings.
- **Never rename or transfer the `unraid-templates` repo** or your GitHub
  username: CA treats that as a security problem and can blacklist the repo.
