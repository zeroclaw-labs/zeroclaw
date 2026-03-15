# Google Maps Places Skill — Design Spec

**Date:** 2026-03-15
**Status:** Reviewed
**Skill name:** `gmaps-places`

## Purpose

Live Google Places API tool for ZeroClaw agent. On-demand search and detail retrieval for cafes, restaurants, and other businesses worldwide. No persistent storage — agent combines results with other tools (ERP, Telegram, etc.) at query time.

**Use cases:**
1. Competitive analysis — ratings, reviews, positioning of competitors in any zone
2. B2B lead generation — collect contacts (phone, website, socials) of businesses by area
3. Market research — aggregate data on business density, ratings, price levels by zone

## Architecture

```
User question
  → ZeroClaw agent
    → gmaps_search(query, location, radius, type)  → list of places
    → gmaps_details(place_id)                       → full place info + reviews
  → Agent formats report (combines with erp_sales, telegram, etc.)
  → User gets answer
```

No database. No caching layer. Pure request-response tools. Two tools only (`gmaps_compare` dropped — agent can call `gmaps_details` N times and compare natively).

## Tools

### `gmaps_search`

Uses **Text Search** (`POST /v1/places:searchText`) exclusively. Nearby Search dropped — Text Search covers all use cases with optional `locationBias` for geographic focus.

**Args:**
| Arg | Required | Description |
|-----|----------|-------------|
| `query` | yes | Search text: "coffee shops in Samui", "restaurants Chaweng" |
| `type` | no | Place type filter: cafe, restaurant, bar, hotel, spa (default: none) |
| `radius` | no | Radius in meters for locationBias (default: 5000). Only used when `location` is set |
| `location` | no | Lat,lng center for locationBias. If omitted, Google infers from query text |
| `min_rating` | no | Client-side post-filter: only places with rating >= N (default: 0). May return fewer than `limit` results |
| `limit` | no | Max results to return (default: 20, max: 60 via automatic pagination) |
| `sort_by` | no | Client-side sort: relevance (default, API-native), rating, reviews_count. Note: rating/reviews_count sorting is applied post-fetch on the returned set only |
| `page_token` | no | Pagination token from previous response to fetch next page |
| `language` | no | Language for results: en (default), ru, or local language of target country (es for Spain, vi for Vietnam, th for Thailand, etc.). Agent should pick local language based on search zone |

**Returns:**
```json
{
  "success": true,
  "count": 15,
  "next_page_token": "AeJbb3...",
  "places": [
    {
      "place_id": "ChIJ...",
      "name": "Cafe XYZ",
      "address": "123 Beach Rd, Samui",
      "location": {"lat": 9.53, "lng": 100.06},
      "rating": 4.5,
      "reviews_count": 128,
      "price_level": 2,
      "types": ["cafe", "food"],
      "open_now": true
    }
  ]
}
```

**Implementation notes:**
- `min_rating` is a client-side post-filter (Places API has no native rating filter). When set, client fetches extra results (up to 60) then filters and truncates to `limit`.
- `sort_by=rating|reviews_count` is client-side sort via `gmaps_queries.py`. Only `relevance` maps to API's `rankPreference=RELEVANCE`.
- `open_now` is extracted from nested `currentOpeningHours.openNow` boolean in API response.
- Pagination: client handles `nextPageToken` internally when `limit > 20`. Returns `next_page_token` for manual pagination via `page_token` arg.

**FieldMask** (cost optimization — Basic tier): `places.id,places.displayName,places.formattedAddress,places.location,places.rating,places.userRatingCount,places.priceLevel,places.types,places.currentOpeningHours`

### `gmaps_details`

Full place details by place_id.

**Args:**
| Arg | Required | Description |
|-----|----------|-------------|
| `place_id` | yes | Google Place ID from search results |
| `reviews` | no | Include review texts (default: true) |
| `photos` | no | Include photo URLs — resolved to direct URLs internally, costs ~$0.007/photo (default: false) |
| `max_photos` | no | Max photos to resolve (default: 3) |
| `language` | no | Language for results: en (default), ru, or local language of target country (es for Spain, vi for Vietnam, th for Thailand, etc.). Agent should pick local language based on search zone |

**Returns:**
```json
{
  "success": true,
  "place": {
    "place_id": "ChIJ...",
    "name": "Cafe XYZ",
    "address": "123 Beach Rd, Samui",
    "location": {"lat": 9.53, "lng": 100.06},
    "rating": 4.5,
    "reviews_count": 128,
    "price_level": 2,
    "types": ["cafe", "food"],
    "phone": "+66-77-123456",
    "website": "https://cafexyz.com",
    "google_maps_url": "https://maps.google.com/?cid=...",
    "hours": ["Mon: 8:00-22:00", "Tue: 8:00-22:00"],
    "reviews": [
      {"author": "John", "rating": 5, "text": "Great coffee!", "time": "2026-02-10", "language": "en"}
    ],
    "photos": [
      {"url": "https://lh3.googleusercontent.com/...", "width": 1200, "height": 800}
    ]
  }
}
```

**Photo resolution:** API returns `photos[].name` resource path. Client resolves each to a direct URL via `GET /v1/{name}/media?maxWidthPx=800&key=...` (redirect URL). Cost: ~$0.007 per photo. With `max_photos=3`, that's 3 extra API calls.

**FieldMask** (Advanced tier when reviews requested): `id,displayName,formattedAddress,location,rating,userRatingCount,priceLevel,types,nationalPhoneNumber,internationalPhoneNumber,websiteUri,googleMapsUri,currentOpeningHours,reviews,photos`

## Implementation

### File structure

```
~/.zeroclaw/workspace/skills/gmaps-places/
├── SKILL.toml           # Tool definitions + agent prompt
├── scripts/
│   ├── gmaps_client.py  # Google Places API (New) client + daily budget counter
│   ├── gmaps_queries.py # Pure functions: filtering, sorting, response parsing
│   └── gmaps_places.py  # CLI entrypoint (argparse subcommands)
└── tests/
    ├── conftest.py
    ├── test_client.py    # API response parsing tests (mocked)
    └── test_queries.py   # Pure function tests
```

Same pattern as `erp-analyst`: client / queries (pure) / CLI entrypoint.

### SKILL.toml draft

```toml
[skill]
name = "gmaps-places"
description = "Google Maps Places API: search businesses, get ratings/reviews/contacts, competitive analysis worldwide"
version = "1.0.0"
author = "ZeroClaw User"
tags = ["google-maps", "places", "competitive-analysis", "restaurants", "cafes"]

prompts = [
  """
  Google Maps Places tool — search for businesses, get details, reviews, and contacts worldwide.

  TOOLS:
  1. gmaps_search(query, type, location, radius, min_rating, limit, sort_by, language)
     — Search for places by text query with optional geographic focus
  2. gmaps_details(place_id, reviews, photos, language)
     — Get full details for a specific place: contacts, hours, reviews, photos

  WORKFLOW:
  1. Use gmaps_search to find places matching the user's query
  2. Use gmaps_details for specific places when user needs contacts, reviews, or photos
  3. Combine with erp_sales/erp_expenses for competitive analysis against own data

  LANGUAGE: Set `language` based on the TARGET COUNTRY of the search:
  - Spain → es, Vietnam → vi, Thailand → th, Japan → ja, France → fr, etc.
  - Always keep en and ru available (user speaks both)
  - Default: en. Override with local language to get local business names/addresses.

  COST AWARENESS: Every call costs real money. Prefer search over details when possible.
  Do not call gmaps_details in a loop for all search results — only for places the user cares about.

  Respond in the SAME LANGUAGE as the user's question.
  """
]

[[tools]]
name = "gmaps_search"
description = "Search Google Maps for businesses. Returns: name, rating, reviews count, address, price level, types. Use for finding competitors, leads, market research."
kind = "shell"
command = "python3 ~/.zeroclaw/workspace/skills/gmaps-places/scripts/gmaps_places.py search --query {query} --type {type} --location {location} --radius {radius} --min-rating {min_rating} --limit {limit} --sort-by {sort_by} --page-token {page_token} --language {language}"
max_result_chars = 4000
max_calls_per_turn = 3

[tools.args]
query = "Search text: 'coffee shops in Samui', 'restaurants Chaweng', etc."
type = "Place type filter: cafe, restaurant, bar, hotel, spa (optional)"
location = "Lat,lng for geographic focus: '9.53,100.06' (optional, inferred from query)"
radius = "Radius in meters (default: 5000, only with location)"
min_rating = "Min rating filter, client-side (default: 0)"
limit = "Max results (default: 20, max: 60)"
sort_by = "Sort: relevance (default), rating, reviews_count"
page_token = "Pagination token from previous response (optional)"
language = "Result language: en (default), ru, or local (es/vi/th/ja/fr/etc. — pick by target country)"

[[tools]]
name = "gmaps_details"
description = "Full place details by Place ID. Returns: contacts (phone, website), hours, reviews, photos. Use after gmaps_search for specific places."
kind = "shell"
command = "python3 ~/.zeroclaw/workspace/skills/gmaps-places/scripts/gmaps_places.py details --place-id {place_id} --reviews {reviews} --photos {photos} --max-photos {max_photos} --language {language}"
max_result_chars = 4000
max_calls_per_turn = 5

[tools.args]
place_id = "Google Place ID from search results (required)"
reviews = "Include reviews: true (default) or false"
photos = "Include photo URLs: true or false (default). Costs ~$0.007/photo extra"
max_photos = "Max photos to fetch (default: 3)"
language = "Result language: en (default), ru, or local (es/vi/th/ja/fr/etc. — pick by target country)"
```

### API: Google Places API (New)

Uses `https://places.googleapis.com/v1/` endpoints only:

- **Text Search**: `POST /v1/places:searchText`
- **Place Details**: `GET /v1/places/{place_id}`
- **Place Photos**: `GET /v1/places/{place_id}/photos/{photo_reference}/media`

Auth: `X-Goog-Api-Key` header with `GOOGLE_API_KEY` env var.

Nearby Search out of scope — Text Search with `locationBias` covers all use cases.

### Cost control

Places API (New) charges per field requested. Strategy:
- Search: minimal FieldMask (Basic tier, no reviews/photos) — cheapest
- Details: Advanced tier only when reviews requested
- Photos: resolved on-demand, each costs ~$0.007
- `max_result_chars = 4000` in SKILL.toml truncates oversized responses
- `max_calls_per_turn` limits agent from firing too many calls

**Daily budget cap:** `gmaps_client.py` maintains a simple file-based counter at `/tmp/gmaps_daily_calls.json`. Default limits:
- 100 search calls/day (~$3.20)
- 50 detail calls/day (~$1.25)
- 30 photo calls/day (~$0.21)
- Total max: ~$4.66/day

When limit is reached, tool returns `{"success": false, "error": "Daily API budget reached (100/100 search calls). Resets at midnight."}`.

Estimated cost per query type:
- Text Search (Basic): ~$0.032 per request
- Place Details (Advanced, with reviews): ~$0.025 per place
- Photos: ~$0.007 per photo

### Config

**Environment:** `GOOGLE_API_KEY` added to `shell_env_passthrough` and `auto_approve` in config.toml.

**Agent config** (`config.toml`): add `gmaps_search`, `gmaps_details` to erp_analyst's `allowed_tools` so CFO agent can combine financial + competitive data. Also usable standalone by any agent.

### Dependencies

- `requests` (already available — used by erp-analyst)
- No additional packages needed

### Error handling

- Missing `GOOGLE_API_KEY` → exit 0 with `{"success": false, "error": "GOOGLE_API_KEY not set"}`
- API errors (quota, invalid key) → exit 0 with error message (soft failure, same pattern as erp-analyst)
- Daily budget exceeded → exit 0 with budget error
- Zero results → `{"success": true, "count": 0, "places": []}`
- All prints use `flush=True`

### Testing

- `test_queries.py`: pure function tests for filtering (min_rating), sorting (rating, reviews_count), response parsing (open_now extraction from nested struct)
- `test_client.py`: mocked API responses, FieldMask construction, pagination token handling, daily budget counter
- Target: 15+ tests
- No live API tests in CI (cost); manual smoke tests documented

## Agent integration examples

```
User: "Какие кафе рядом с Sweet & Salty и какой у них рейтинг?"
Agent: gmaps_search(query="cafe near Maenam Samui", radius=2000) → list
       → formats competitive overview

User: "Сравни нас с Coco Tam's и The Jungle Club"
Agent: gmaps_search(query="Coco Tam's Samui") → place_id
       gmaps_details(place_id=id1) → details
       gmaps_search(query="The Jungle Club Samui") → place_id
       gmaps_details(place_id=id2) → details
       erp_sales(source=ss, period=month) → own revenue
       → formats comparison report

User: "Найди все рестораны в Чиангмае с рейтингом 4.5+ для партнёрства"
Agent: gmaps_search(query="restaurant Chiang Mai", min_rating=4.5, limit=40)
       → formats lead list with contacts
```

## Out of scope

- Persistent storage / caching
- Nearby Search endpoint (Text Search with locationBias is sufficient)
- `gmaps_compare` tool (agent calls details + compares natively)
- Scheduled monitoring (cron) — possible later as separate concern
- Google Maps JavaScript API / frontend
- Photo downloading (only resolved URLs returned)
- Review sentiment analysis
