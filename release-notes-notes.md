## release-runbook
- "tag exists on" -> should be "upstream"

## Add version bump PR
- There's a Skill

Needs to be upstream:

git fetch origin
git show origin/master:Cargo.toml | grep '^version'
# Must show: version = "X.Y.Z"


`https://github.com/zeroclaw-labs/zeroclaw/actions/workflows/release-stable-manual.yml
` should be an href <>


Should be a Markdown checklist:

```
```
```
```
- [ ] GitHub Release exists at /releases/tag/vX.Y.Z and is marked Latest
- [ ] Release notes are non-empty
- [ ] SHA256SUMS asset is present and non-empty
- [ ] At least one binary archive is downloadable (spot-check linux x86_64)
- [ ] CHANGELOG-next.md is gone from master (the publish job removes it automatically)
```
```
```
