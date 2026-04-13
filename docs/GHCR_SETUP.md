# GitHub Container Registry Setup Guide

This guide explains how to enable automatic Docker image builds and publishing to GitHub Container Registry (GHCR).

## What This Does

- Automatically builds Docker images when you push code
- Publishes images to `ghcr.io/your-username/loradb`
- Supports multi-architecture (amd64 and arm64)
- Users can pull pre-built images instead of building locally
- **Zero build time for end users on low-powered devices**

## One-Time Setup

### 1. Enable GitHub Actions (if not already enabled)

GitHub Actions should be enabled by default. Verify at:
```
https://github.com/YOUR_USERNAME/loradb/settings/actions
```

### 2. Configure Package Permissions

The workflow file (`.github/workflows/docker-publish.yml`) is already configured with the necessary permissions:
```yaml
permissions:
  contents: read
  packages: write
  id-token: write
```

No manual token creation needed - GitHub Actions automatically uses `GITHUB_TOKEN`.

### 3. Make Repository Public (or configure private access)

**For public images (recommended for open source):**
- Make your repository public
- Images will be publicly pullable by anyone

**For private images:**
- Repository can stay private
- Users need authentication to pull images:
  ```bash
  echo $GITHUB_TOKEN | docker login ghcr.io -u USERNAME --password-stdin
  docker pull ghcr.io/username/loradb:latest
  ```

### 4. Push Code to Trigger Build

The workflow triggers on:
- **Version tags**: `git tag v0.1.0 && git push --tags`
- **Main branch**: `git push origin main`
- **Pull requests**: Automatic build (no publish)
- **Manual**: Via GitHub Actions UI

## Usage

### Triggering Automatic Builds

**Create a release (recommended):**
```bash
# Tag a new version
git tag v0.1.0
git push --tags

# This triggers a build and publishes:
# - ghcr.io/username/loradb:v0.1.0
# - ghcr.io/username/loradb:0.1
# - ghcr.io/username/loradb:0
# - ghcr.io/username/loradb:latest
```

**Push to main branch:**
```bash
git push origin main

# This publishes:
# - ghcr.io/username/loradb:main
# - ghcr.io/username/loradb:latest
# - ghcr.io/username/loradb:sha-abc123
```

### Viewing Published Images

Visit: `https://github.com/YOUR_USERNAME?tab=packages`

Or view your repository's packages:
```
https://github.com/YOUR_USERNAME/loradb/pkgs/container/loradb
```

### Using Pre-built Images

**With docker-compose:**
```bash
# Update docker-compose.prebuilt.yml with your username
docker-compose -f docker-compose.prebuilt.yml up -d
```

**Direct pull:**
```bash
docker pull ghcr.io/YOUR_USERNAME/loradb:latest
```

## Monitoring Builds

### View Build Status

1. Go to your repository
2. Click "Actions" tab
3. See all workflow runs and their status

### Build Badge (optional)

Add to README.md:
```markdown
![Docker Build](https://github.com/YOUR_USERNAME/loradb/actions/workflows/docker-publish.yml/badge.svg)
```

## Architecture Support

The workflow automatically builds for:
- **linux/amd64** - x86_64 systems (Intel/AMD servers, desktops)
- **linux/arm64** - ARM64 systems (Raspberry Pi 3/4/5, Apple Silicon, AWS Graviton)

Docker automatically pulls the correct architecture for the user's device.

## Build Times

**First build:** 10-15 minutes (full Rust compilation)
**Subsequent builds:** 5-10 minutes (with layer caching)

Users get **zero build time** - they just pull pre-built images.

## Troubleshooting

### Build fails with "permission denied"

Ensure the workflow has `packages: write` permission (already configured in `.github/workflows/docker-publish.yml`).

### Images not showing as public

1. Go to package settings: `https://github.com/users/YOUR_USERNAME/packages/container/loradb/settings`
2. Under "Danger Zone", change visibility to "Public"

### Build takes too long

This is normal for Rust projects. First build is slow, but:
- GitHub Actions runners are powerful (no cost to you)
- Layer caching speeds up subsequent builds
- End users never build - they just pull

## Cost

**Free tier includes:**
- ✅ Unlimited public image storage
- ✅ Unlimited public image pulls
- ✅ 2,000 GitHub Actions minutes/month
- ✅ 20 concurrent jobs

**LoRaDB typical usage:**
- ~10 minutes per build
- ~200 builds/month = ~100% of free tier

**Compared to Docker Hub free tier:**
- Docker Hub: Limited pulls, build minutes, image retention
- GHCR: Unlimited for public repos

## Alternative: Docker Hub

To publish to Docker Hub instead, modify `.github/workflows/docker-publish.yml`:

```yaml
- name: Log in to Docker Hub
  uses: docker/login-action@v3
  with:
    username: ${{ secrets.DOCKERHUB_USERNAME }}
    password: ${{ secrets.DOCKERHUB_TOKEN }}

- name: Extract metadata
  id: meta
  uses: docker/metadata-action@v5
  with:
    images: username/loradb  # Docker Hub image name
```

Then add secrets at: `https://github.com/YOUR_USERNAME/loradb/settings/secrets/actions`

## Next Steps

1. Push a version tag to trigger first build
2. Update `docker-compose.prebuilt.yml` with your username
3. Share pre-built image URL with users
4. Add build badge to README (optional)

Users can now pull pre-built images instead of building locally!
