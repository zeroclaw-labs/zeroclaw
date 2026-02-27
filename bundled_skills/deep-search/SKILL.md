# Deep Search

Iterative search with ReAct pattern for comprehensive research.

## When to Use

- User asks to "research" or "find all information about" a topic
- Initial search results seem incomplete
- Multi-faceted topics requiring multiple perspectives

## Search Strategy

1. **Initial Search**: Use web_search with the user's original query
2. **Analyze Results**: 
   - Identify gaps in coverage
   - Note related topics mentioned
3. **Refine Query**: Construct better search terms based on gaps
4. ## Iterate**: Repeat with refined queries (max 3-5 iterations)
5. **Synthesize**: Combine all findings into comprehensive response

## Search Patterns

### Broad-to-Narrow
Start with broad terms, narrow down based on results.

### Multi-Pedium
Search from different angles (technical, business, historical, etc).

### Source-Diversification
Use multiple search terms to cross-reference.

## Output Format

Always include:
- Summary of key findings
- Confidence level (high/medium/low)
- Sources or search terms used
- Suggested follow-up if applicable
