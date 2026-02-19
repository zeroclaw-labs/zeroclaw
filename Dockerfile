# --- Build stage ---
FROM node:22-alpine AS builder
WORKDIR /app

# Skip Playwright browser download (not needed for production)
ENV PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1

COPY package.json package-lock.json .npmrc ./
RUN npm ci

COPY . .
RUN npm run build

# --- Fetch built-in OpenClaw skills ---
FROM node:22-alpine AS skills
WORKDIR /tmp
RUN apk add --no-cache git && \
    npm install --global --ignore-scripts openclaw@latest
# Copy just the skills directory
RUN SKILLS_DIR=$(npm root -g)/openclaw/skills && \
    mkdir -p /openclaw-skills && \
    if [ -d "$SKILLS_DIR" ]; then cp -r "$SKILLS_DIR"/* /openclaw-skills/; fi

# --- Production stage ---
FROM node:22-alpine AS runner
WORKDIR /app

ENV NODE_ENV=production

# Create non-root user
RUN addgroup -S clawsuite && adduser -S clawsuite -G clawsuite

# Copy build output and package.json (for any runtime deps)
COPY --from=builder /app/dist ./dist
COPY --from=builder /app/node_modules ./node_modules
COPY --from=builder /app/package.json ./
COPY --from=builder /app/server-entry.js ./

# Copy built-in OpenClaw skills
COPY --from=skills /openclaw-skills ./openclaw-skills

# Expose default port
EXPOSE 3000

USER clawsuite

CMD ["node", "server-entry.js"]
