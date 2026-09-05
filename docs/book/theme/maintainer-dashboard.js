(function () {
    "use strict";

    var root = document.getElementById("maintainer-dashboard-app");
    if (!root) {
        return;
    }

    var repository = root.dataset.repository;
    var statusNode = root.querySelector("[data-dashboard-status]");
    var reviewNode = root.querySelector("[data-dashboard-review]");
    var projectNode = root.querySelector("[data-dashboard-project]");
    var zt5Node = root.querySelector("[data-dashboard-zt5]");
    var controls = root.querySelector("[data-dashboard-controls]");
    var actorInput = controls.querySelector("[name=actor]");
    var refreshButton = controls.querySelector("[data-dashboard-refresh]");
    var params = new URLSearchParams(window.location.search);
    var renderGeneration = 0;
    var searchCache = new Map();
    var searchCacheTtlMs = 60 * 1000;
    var searchTimeoutMs = 10 * 1000;

    actorInput.value = validLogin(params.get("actor")) ? params.get("actor") : "";

    function validLogin(value) {
        return typeof value === "string" && /^[A-Za-z0-9](?:[A-Za-z0-9-]{0,37}[A-Za-z0-9])?$/.test(value);
    }

    function githubSearchUrl(query, kind) {
        var path = kind === "pr" ? "pulls" : "issues";
        return "https://github.com/" + repository + "/" + path + "?q=" + encodeURIComponent(query);
    }

    function element(tag, className, text) {
        var node = document.createElement(tag);
        if (className) {
            node.className = className;
        }
        if (text !== undefined) {
            node.textContent = text;
        }
        return node;
    }

    function addLink(parent, label, url) {
        var link = element("a", "", label);
        link.href = url;
        link.rel = "noreferrer";
        parent.appendChild(link);
    }

    function renderPending(container, definitions) {
        container.replaceChildren();
        definitions.forEach(function (definition) {
            var card = element("article", "maintainer-dashboard-card");
            card.dataset.dashboardKey = definition.key;
            card.appendChild(element("h3", "", definition.title));
            card.appendChild(element("p", "maintainer-dashboard-count", "…"));
            card.appendChild(element("p", "maintainer-dashboard-detail", definition.detail));
            addLink(card, "Open GitHub search", githubSearchUrl(definition.query, definition.kind));
            container.appendChild(card);
        });
    }

    function renderResult(container, definition, result) {
        var card = container.querySelector('[data-dashboard-key="' + definition.key + '"]');
        var count = card.querySelector(".maintainer-dashboard-count");
        count.textContent = result.ok ? String(result.total) : "Unavailable";
        count.classList.toggle("maintainer-dashboard-count-error", !result.ok);

        var oldList = card.querySelector("ul");
        if (oldList) {
            oldList.remove();
        }
        if (!result.ok || result.items.length === 0) {
            return;
        }
        var list = element("ul", "maintainer-dashboard-items");
        result.items.forEach(function (item) {
            var row = element("li");
            var label = "#" + item.number + " " + item.title;
            addLink(row, label, item.html_url);
            list.appendChild(row);
        });
        card.insertBefore(list, card.lastElementChild);
    }

    function publicQueries(actor) {
        var base = "repo:" + repository;
        var routed = ' is:pr is:open draft:false label:"needs-maintainer-review" -label:"needs-author-action" -label:"status:blocked" -label:"do-not-merge" -label:stacked';
        var review = [
            {key: "near-ready", title: "Near-ready", kind: "pr", query: base + routed + " status:success", detail: "Successful-check candidates routed for maintainer review."},
            {key: "maintainer", title: "Maintainer review", kind: "pr", query: base + routed, detail: "Open PRs routed to maintainers."},
            {key: "second-core", title: "Potential second Core", kind: "pr", query: base + routed + ' label:"risk:high","domain:security" review:approved', detail: "Possible high-risk or security PRs needing exact-head Core review inspection."},
            {key: "author-action", title: "Author action", kind: "pr", query: base + ' is:pr is:open draft:false label:"needs-author-action" -label:"status:blocked" -label:"do-not-merge"', detail: "Label-backed backlog; use the CLI for unanswered-request age."},
            {key: "stacked", title: "Stacked", kind: "pr", query: base + " is:pr is:open draft:false label:stacked", detail: "Open non-draft dependent PRs."}
        ];
        var project = [
            {key: "roadmap", title: "Roadmap", kind: "issue", query: base + " is:issue is:open -no:milestone", detail: "Open issues carrying a milestone."},
            {key: "board", title: "Active board", kind: "issue", query: base + ' is:issue is:open label:"status:in-progress","status:blocked"', detail: "In-progress or explicitly blocked issue work."},
            {key: "backlog", title: "Accepted backlog", kind: "issue", query: base + ' is:issue is:open label:"status:accepted" -label:"status:in-progress"', detail: "Accepted issues without the active-PR signal."}
        ];
        if (actor) {
            project.push({key: "my-work", title: "My work", kind: "issue", query: base + " is:issue is:open assignee:" + actor, detail: "Open issues assigned to @" + actor + "."});
            review.push({key: "my-prs", title: "My PRs", kind: "pr", query: base + routed + " author:" + actor, detail: "Maintainer-review candidates authored by @" + actor + "."});
        }
        return {review: review, project: project};
    }

    function search(definition) {
        var cached = searchCache.get(definition.query);
        if (cached && cached.expiresAt > Date.now()) {
            return cached.promise;
        }

        var endpoint = "https://api.github.com/search/issues?q=" + encodeURIComponent(definition.query) + "&per_page=5";
        var controller = new AbortController();
        var timeout = window.setTimeout(function () {
            controller.abort();
        }, searchTimeoutMs);
        var promise = (async function () {
            try {
                var response = await fetch(endpoint, {
                    headers: {Accept: "application/vnd.github+json"},
                    signal: controller.signal
                });
                if (!response.ok) {
                    throw new Error("GitHub returned " + response.status);
                }
                var payload = await response.json();
                if (!Number.isInteger(payload.total_count) || !Array.isArray(payload.items)) {
                    throw new Error("GitHub returned an unexpected response");
                }
                return {ok: true, total: payload.total_count, items: payload.items.slice(0, 5)};
            } catch (error) {
                return {ok: false, total: null, items: [], error: String(error)};
            } finally {
                window.clearTimeout(timeout);
            }
        }());
        searchCache.set(definition.query, {expiresAt: Date.now() + searchCacheTtlMs, promise: promise});
        return promise;
    }

    async function renderQueries() {
        var generation = ++renderGeneration;
        var actor = validLogin(actorInput.value.trim()) ? actorInput.value.trim() : "";
        var definitions = publicQueries(actor);
        renderPending(reviewNode, definitions.review);
        renderPending(projectNode, definitions.project);
        statusNode.textContent = "Refreshing public GitHub data…";
        var all = definitions.review.concat(definitions.project);
        var results = await Promise.all(all.map(search));
        if (generation !== renderGeneration) {
            return;
        }
        var failures = 0;
        all.forEach(function (definition, index) {
            var container = definition.kind === "pr" ? reviewNode : projectNode;
            renderResult(container, definition, results[index]);
            failures += results[index].ok ? 0 : 1;
        });
        statusNode.textContent = failures === 0
            ? "Public GitHub snapshot. Responses are cached for up to one minute; refresh before making a review or merge decision."
            : failures + " view" + (failures === 1 ? " is" : "s are") + " unavailable. Use the GitHub search links instead.";
    }

    function publicZt5Url(value) {
        try {
            var url = new URL(value, window.location.href);
            return url.origin === window.location.origin ? url.href : null;
        } catch (error) {
            return null;
        }
    }

    function hasExactKeys(value, keys) {
        return value && typeof value === "object" && !Array.isArray(value)
            && Object.keys(value).sort().join("|") === keys.slice().sort().join("|");
    }

    function validPublicZt5Link(link) {
        if (!hasExactKeys(link, ["label", "url"]) || typeof link.label !== "string" || typeof link.url !== "string") {
            return false;
        }
        try {
            var url = new URL(link.url);
            return url.protocol === "https:" && url.hostname === "github.com"
                && /^\/zeroclaw-labs\/zeroclaw\/pull\/\d+$/.test(url.pathname);
        } catch (error) {
            return false;
        }
    }

    function validPublicZt5Capability(capability) {
        return hasExactKeys(capability, ["name", "status", "score", "target", "summary", "links"])
            && typeof capability.name === "string"
            && typeof capability.status === "string"
            && capability.score === null
            && capability.target === 5
            && typeof capability.summary === "string"
            && Array.isArray(capability.links)
            && capability.links.length > 0
            && capability.links.every(validPublicZt5Link);
    }

    function validPublicZt5(payload) {
        return hasExactKeys(payload, ["schema_version", "as_of", "disclosure", "capabilities"])
            && payload.schema_version === 1
            && /^\d{4}-\d{2}-\d{2}$/.test(payload.as_of)
            && typeof payload.disclosure === "string"
            && Array.isArray(payload.capabilities)
            && payload.capabilities.length > 0
            && payload.capabilities.every(validPublicZt5Capability);
    }

    function renderZt5(payload) {
        zt5Node.replaceChildren();
        payload.capabilities.forEach(function (capability) {
            var card = element("article", "maintainer-dashboard-card maintainer-dashboard-zt5-card");
            card.appendChild(element("h3", "", capability.name));
            var score = capability.score === null ? "Not publicly scored" : capability.score + " / " + capability.target;
            card.appendChild(element("p", "maintainer-dashboard-count", score));
            card.appendChild(element("p", "maintainer-dashboard-status", capability.status));
            card.appendChild(element("p", "maintainer-dashboard-detail", capability.summary));
            var links = element("p", "maintainer-dashboard-links");
            capability.links.forEach(function (link, index) {
                if (index > 0) {
                    links.appendChild(document.createTextNode(" · "));
                }
                addLink(links, link.label, link.url);
            });
            card.appendChild(links);
            zt5Node.appendChild(card);
        });
        var freshness = element("p", "maintainer-dashboard-note", "Public ZT5 snapshot as of " + payload.as_of + ". " + payload.disclosure);
        zt5Node.parentNode.insertBefore(freshness, zt5Node);
    }

    async function loadZt5() {
        var url = publicZt5Url(root.dataset.zt5Url);
        if (!url) {
            zt5Node.textContent = "Public Zero-to-5 snapshot unavailable.";
            return;
        }
        try {
            var response = await fetch(url);
            if (!response.ok) {
                throw new Error("snapshot returned " + response.status);
            }
            var payload = await response.json();
            if (!validPublicZt5(payload)) {
                throw new Error("snapshot has an unexpected schema");
            }
            renderZt5(payload);
        } catch (error) {
            zt5Node.textContent = "Public Zero-to-5 snapshot unavailable.";
        }
    }

    controls.addEventListener("submit", function (event) {
        event.preventDefault();
        var actor = actorInput.value.trim();
        if (actor && !validLogin(actor)) {
            statusNode.textContent = "Enter a valid GitHub login.";
            actorInput.focus();
            return;
        }
        var next = new URL(window.location.href);
        if (actor) {
            next.searchParams.set("actor", actor);
        } else {
            next.searchParams.delete("actor");
        }
        window.history.replaceState({}, "", next);
        renderQueries();
    });
    refreshButton.addEventListener("click", renderQueries);

    renderQueries();
    loadZt5();
}());
