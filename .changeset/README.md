# Changesets

This folder is used by [Changesets](https://github.com/changesets/changesets) to track changes and manage versioning.

## Adding a changeset

```bash
pnpm changeset
```

Follow the prompts to select which packages changed and write a summary.

## Releasing

Releases are automated via GitHub Actions. When changesets are merged to `main`, a "Version Packages" PR is created. Merging that PR publishes to npm.
