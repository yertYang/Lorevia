# 从 GitHub 更新 Docker 服务

在服务器上使用 Lorevia 的独立 Git 工作目录，并让部署分支跟踪 GitHub 上的目标分支（默认是 `origin/master`）。首次接入后，管理员在 **设置 → 关于 → Docker 更新** 点击 **开始更新**：系统先核对 GitHub 分支、服务器本地改动和目标提交，再弹出部署确认。确认后会自动拉取代码、构建镜像、替换应用容器并等待健康检查。界面显示阶段进度和最近操作；关闭进度窗口后部署仍会继续。只点击 **检查新提交** 不会修改工作目录或重启应用。即使版本号未变，也能按提交更新。

**首次启用**：已有部署先备份数据，确认原有数据目录和端口；新部署可从 `https://github.com/yertYang/Lorevia.git` 克隆。请在服务器工作目录的 `.env` 中配置 `DEEPTUTOR_REPO`（工作目录的绝对路径）、`DEEPTUTOR_UPDATE_TOKEN`（用 `openssl rand -hex 32` 生成）、`DEEPTUTOR_DATA_DIR` 和 `HOST_PORT_FRONTEND`。新部署默认使用项目名 `lorevia`；接入现有部署时，使用原来的项目名、数据目录和端口，不要同时启动两个实例。

```sh
# 在部署服务器的 Git 工作目录中运行
export DEEPTUTOR_BUILD_REVISION="$(git rev-parse HEAD)"
docker compose -f compose.yaml -f compose.server.yaml up -d --build
```

登录管理员账号后刷新“关于”页面。默认开发容器和仅运行镜像的部署没有更新服务，只显示配置入口。构建失败时原容器继续运行；新容器启动后通过健康检查才算成功。若替换后健康检查失败，需要管理员处理，不会自动回滚。

### 从自己的电脑接入已有服务器（一次即可）

如果服务器已经用本仓库的 `compose.yaml` 部署，并且原有端口和数据目录配置保存在仓库的 `.env` 中，可以从自己的电脑执行下面这条 SSH 命令。替换 SSH 主机、工作目录和 **原有 Compose 项目名**。命令只在首次接入时需要；之后应用代码的检查、更新和重启全部在网页上完成。

```sh
ssh admin@your-server 'cd /srv/Lorevia && sh -s' <<'SETUP'
set -eu
project=lorevia  # 改为原项目名，可在容器的 com.docker.compose.project 标签中查看
test -n "$(docker compose -p "$project" -f compose.yaml ps -q deeptutor)" || {
  echo "没有找到正在运行的 deeptutor 服务，请核对目录和项目名。" >&2; exit 1;
}
test -z "$(git status --porcelain --untracked-files=no)" || {
  echo "工作目录有改动，请先处理后再启用。" >&2; exit 1;
}
git pull --ff-only
umask 077
touch .env
if ! grep -q '^DEEPTUTOR_UPDATE_TOKEN=' .env; then
  token=$(openssl rand -hex 32)
  printf '\nDEEPTUTOR_UPDATE_TOKEN=%s\n' "$token" >> .env
fi
export DEEPTUTOR_REPO="$(pwd -P)"
export DEEPTUTOR_BUILD_REVISION="$(git rev-parse HEAD)"
docker compose -p "$project" -f compose.yaml -f compose.server.yaml up -d --build --wait
SETUP
```

如果服务器只有已经拉好的镜像，没有 Git 工作目录，需要先由管理员将它接入上述源码部署，沿用原数据卷和端口。单独运行的应用容器没有宿主机 Docker 权限，网页无法自行授予该权限。

## Deployment details

Use a dedicated Linux server checkout of the desired Git branch. The branch
must track its remote, and tracked files must be clean. The updater fetches that
upstream, permits only fast-forward merges, builds locally and replaces only the
`deeptutor` service. New deployments default to project `lorevia`; pass the original project name with `-p` when enabling updates on an existing deployment.

Create `.env` in the checkout (keep existing port/data configuration there):

```dotenv
DEEPTUTOR_REPO=/srv/Lorevia
DEEPTUTOR_UPDATE_TOKEN=replace-with-a-random-token-at-least-32-characters-long
HOST_PORT_FRONTEND=3782
DEEPTUTOR_DATA_DIR=./data
```

Generate a token with `openssl rand -hex 32`. The repository must be mounted at
the same absolute path inside the updater as on the host. Configure remote Git
access on the server; public HTTPS remotes work without additional credentials.
Private SSH remotes need separately configured credentials in the updater.

From that checkout, initialize the deployment:

```sh
export DEEPTUTOR_BUILD_REVISION="$(git rev-parse HEAD)"
docker compose -f compose.yaml -f compose.server.yaml up -d --build
```

The default port binds to `127.0.0.1`; expose it through your HTTPS reverse proxy
or connect through an SSH tunnel. On first visit, set up the administrator account.
This override enables authentication; update operations require an administrator.
Back up an existing deployment before switching its Compose project/data path.

Open **Settings → About → Docker updates → Start update → Confirm**. The first click checks the tracked GitHub branch and local checkout. Confirming calls the server API,
which delegates to the updater. The application keeps running during the build,
then restarts; the UI polls the persisted job across that restart. Settings and
data remain in the existing data mount. Commit revisions identify code updates
even when `VERSION` has not changed. Checks fetch the configured upstream branch,
including commits not associated with a published release.

If the build fails, the existing application container remains running. The
checkout may already have advanced, so fix the build problem and retry. A failed
health check after replacement is reported; automatic rollback is not provided.
An interrupted updater job is marked failed on its next start. Job details are
stored in the `update-state` volume. Do not run concurrent external deployments.

```sh
docker compose -f compose.yaml -f compose.server.yaml logs --tail=100 updater
```

The updater has the Docker socket and write access to this dedicated checkout.
It has no published port; keep it on a trusted server network. The main
application has neither the socket nor repository write access. Updating the
updater itself requires running the deployment command above again. The plain
`compose.yaml` and image-only Compose configuration do not enable UI updates.
