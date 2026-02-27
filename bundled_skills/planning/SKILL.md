# Task Planning

Decompose complex user requests into executable subtasks.

## When to Use

- User request involves multiple steps
- Task requires coordination between different tools
- User says "analyze", "research", "build", "create" with complex requirements

## Planning Process

1. **Understand Goal**: Parse the user's intent
2. **Identify Subtasks**: Break into discrete, executable steps
3. **Order Dependencies**: Determine which steps must first
4. **Assign Tools**: Identify which tools to use for each step
5. **Define Success**: How to know when each step is complete

## Plan Output Format

```markdown
## Plan: [Task Name]

**Goal**: [One sentence summary]

### Subtasks:
1. [Step 1] - Tool: [tool_name]
2. [Step 2] - Tool: [tool_name]
...

### Dependencies:
- Step 2 depends on Step 1 results
- Step 3 can run in parallel with Step 2

### Success Criteria:
- [ ] Criterion 1
- [ ] Criterion 2
```

## Revision

If initial plan doesn't work, revise based on:
- Tool execution results
- New information discovered
- User feedback
