# Maintain the Hugging Face benchmark mirror

The stable destination is `hranesscom/gobstopper-benchmarks`, repository type `dataset`. `hf/README.md` is the dataset card; `hf/manifest.json` is the complete artifact allowlist. The stage command has no network or upload behavior.

## Prepare an update

1. Finish and publish the upstream benchmark through normal repository review and validation. Preserve each experiment's original suite, scorer, protocol, sample selection, exclusions, failure counts, and implementation provenance. Use a new version folder for changed methodology or corrected results. Never overwrite historical results to make them describe a newer release.
2. Select public artifacts individually. Add their exact relative source paths, matching `data/` destinations, and SHA-256 hashes to `hf/manifest.json`. No directory discovery, ignored results, credentials, private sessions, or third-party response archives. An existing public web file still requires redistribution review before adding it to the mirror. The owner approved CC BY 4.0 for the currently selected aggregate reports and protocols on 2026-09-27. Keep the scoped license and attribution in the card. Review rights for each new artifact; do not infer a data license from a code license.
3. Keep the card's links and limitations current. New release tags alone do not require new benchmark data. Preserve stable dataset URLs and append completed studies only.
4. Run `python3 hf/stage.py` and `python3 -m unittest discover -s hf -p 'test_*.py'`. Run repository-required checks, review and merge the change. Record the final source SHA. The card was drafted by Codex; record its independent reviewer in the PR before publication.
5. From a clean checkout of the merged commit, create a new staging folder:

   ```sh
   python3 hf/stage.py --output /tmp/gobstopper-hf-reviewed-export
   ```

   This refuses an existing output folder, changed hashes, symlinks, unsafe paths, unexpected destinations, and a dirty checkout. Inspect the staged card, file list, and `export-manifest.json`. The folder contains only those reviewed files.

## Publish an approved update

The initial browser handoff requires showing Ben the exact public card and artifact manifest and receiving his explicit approval before public publication. That requirement remains in force until he changes it. Routine repository commits do not constitute approval to publish this dataset.

Use the official CLI pinned to `huggingface-hub==2.0.0` with `uvx --from huggingface-hub==2.0.0 hf` (substitute that prefix for `hf` below). Its upload command has no dry-run option; local staging is the review preview. Run `hf auth whoami` to verify the authorized account and organization membership. Do not print or create credentials or assume a signed-in browser authenticates the CLI. If CLI authority is missing, ask the owner to establish a supported approved credential route. An agent must not add a long-lived personal token to avoid authentication.

After exact publication approval and verification that all added artifacts have reviewed rights, with the dataset repository already created under the approved owner:

```sh
hf upload hranesscom/gobstopper-benchmarks /tmp/gobstopper-hf-reviewed-export . --repo-type dataset --commit-message "Sync reviewed benchmark artifacts"
```

Never upload the repository root, use `--delete`, or include raw private records. Uploads preserve older remote files; check the remote file list and existing hashes first so a collision cannot silently overwrite a historical artifact. If the remote changed after review, reconcile before uploading. For concurrent writers, use the official Hub API's parent-commit condition or serialize updates; the CLI invocation above alone is not a concurrency lock.

After upload, record the resulting Hub commit URL in the PR or delivery record. Download the exact revision using `hf download ... --repo-type dataset --revision <commit>` into a new folder and compare every staged file's SHA-256, including the card and export manifest. Verify the rendered public card, primary website link, and downloads. If an upload times out, inspect the Hub history before retrying. Roll back through a reviewed forward correction, keeping historical data and the previous revision addressable.

There is no unattended scheduled publisher yet. Adding one requires an approved machine authentication route, immutable source selection, remote concurrency protection, and the same exact staged-artifact checks. Do not claim autonomous Hub sync merely because staging is automated.
