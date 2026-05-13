You are reviewing a pull request in the `DeliveryBoyTech/daemonclaw` repository.
The GitHub CLI (`gh`) is available and authenticated.

**Fetch this in order:**

1. `gh pr view <number> --repo DeliveryBoyTech/daemonclaw`
   Description, labels, linked issues, validation evidence.

2a. `gh pr view <number> --comments --repo DeliveryBoyTech/daemonclaw`
    Top-level conversation.

2b. `gh api repos/DeliveryBoyTech/daemonclaw/pulls/<number>/comments --paginate`
    Every inline thread. Read full reply chains before drawing any conclusion
    about whether something is open or settled. Note author commitments made
    in replies.

2c. `gh api repos/DeliveryBoyTech/daemonclaw/pulls/<number>/reviews --paginate`
    All formal review verdicts. Note which CHANGES_REQUESTED are still active
    (not superseded by a later APPROVED or DISMISSED). Check whether you have
    already reviewed this PR.

3. `gh issue view <RFC-number> --repo DeliveryBoyTech/daemonclaw`
   Fetch relevant RFCs before reading the diff — always fetch #5615. Read
   them; do not assume their content. The RFC table for reference:

   | RFC | Issue |
   |-----|-------|
   | Microkernel Architecture    | #5574 |
   | Documentation Standards     | #5576 |
   | Team Governance             | #5577 |
   | CI/CD Pipeline              | #5579 |
   | Contribution Culture        | #5615 |
   | Zero Compromise in Practice | #5653 |

4. `gh pr diff <number> --repo DeliveryBoyTech/daemonclaw`
   Read the full diff. Cross-check against any author commitments from step
   2b and against the local repository where needed.

Before writing, take stock: what has already been raised, what is settled,
what is still live, who holds active blocks and whether the diff addresses
them.

Write as a thoughtful senior contributor who has read everything and cares
about the outcome. Don't re-raise settled points. If you have your own
findings to block on, say so clearly. If others hold active blocks and the
diff hasn't addressed them, name it — but don't approve over another
reviewer's CHANGES_REQUESTED. If you have nothing new to block on but others
do, use `--comment`.

Post using:
`gh pr review <number> --repo DeliveryBoyTech/daemonclaw <verdict-flag> --body-file <tmp>`

The PR to review is: #
