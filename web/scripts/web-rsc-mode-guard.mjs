import fs from "node:fs";
import { builtinModules } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";
import { createServer, loadConfigFromFile } from "vite";

const arrayIsArray = Array.isArray;
const objectEntries = Object.entries;
const objectGetPrototypeOf = Object.getPrototypeOf;
const objectHasOwn = Object.hasOwn;
const mapHas = Map.prototype.has.call.bind(Map.prototype.has);
const setHas = Set.prototype.has.call.bind(Set.prototype.has);
const weakSetHas = WeakSet.prototype.has.call.bind(WeakSet.prototype.has);
const {
  basename: pathBasename,
  dirname: pathDirname,
  extname: pathExtname,
  isAbsolute: pathIsAbsolute,
  join: pathJoin,
  relative: pathRelative,
  resolve: pathResolve,
  sep: pathSep,
} = path;
const fsExistsSync = fs.existsSync;
const fsLstatSync = fs.lstatSync;
const fsReadFileSync = fs.readFileSync;
const fsReaddirSync = fs.readdirSync;
const fsRealpathSync = fs.realpathSync;
const fsStatSync = fs.statSync;

const errorPrefix = "web-rsc-mode-guard:";
const scriptPath = fileURLToPath(import.meta.url);
const defaultRepoRoot = pathResolve(pathDirname(scriptPath), "../..");
const scannedExtensions = new Set([
  ".js",
  ".jsx",
  ".mjs",
  ".cjs",
  ".ts",
  ".tsx",
  ".mts",
  ".cts",
]);
const unsupportedExecutableExtensions = new Set([".mdx"]);
const inertSourceExtensions = new Set([
  ".avif",
  ".css",
  ".eot",
  ".gif",
  ".ico",
  ".jpeg",
  ".jpg",
  ".json",
  ".less",
  ".mp3",
  ".mp4",
  ".ogg",
  ".otf",
  ".png",
  ".sass",
  ".scss",
  ".svg",
  ".ttf",
  ".wav",
  ".webm",
  ".webp",
  ".woff",
  ".woff2",
]);
const nodeBuiltins = new Set(
  builtinModules.flatMap((name) => [name, `node:${name}`]),
);
const allowedTransitiveImports = new Set(["@codemirror/theme-one-dark"]);
const allowedReactRouterDomValueExports = new Set([
  "BrowserRouter",
  "Link",
  "MemoryRouter",
  "NavLink",
  "Navigate",
  "Outlet",
  "Route",
  "Routes",
  "useLocation",
  "useNavigate",
  "useParams",
  "useSearchParams",
]);
const forbiddenRscIdentifiers = new Set([
  "RSCHydratedRouter",
  "unstable_RSCHydratedRouter",
  "RSCStaticRouter",
  "unstable_RSCStaticRouter",
  "createCallServer",
  "unstable_createCallServer",
  "getRSCStream",
  "unstable_getRSCStream",
  "matchRSCServerRequest",
  "unstable_matchRSCServerRequest",
  "routeRSCServerRequest",
  "unstable_routeRSCServerRequest",
  "reactRouterRSC",
  "unstable_reactRouterRSC",
]);
const forbiddenCodeGenerationIdentifiers = new Set(["eval", "Function"]);
const timerNames = new Set(["setTimeout", "setInterval"]);
const globalObjectNames = new Set(["globalThis", "self", "window"]);
const expectedVitePluginNames = [
  "vite:react-babel",
  "vite:react:refresh-wrapper",
  "vite:react:config-post",
  "vite:react-refresh-fbm",
  "vite:react-refresh",
  "vite:react-virtual-preamble",
  "@tailwindcss/vite:scan",
  "@tailwindcss/vite:generate:serve",
  "@tailwindcss/vite:generate:build",
  "zeroclaw-dev-app-prefix",
];
const serveOnlyVitePluginName = "zeroclaw-dev-app-prefix";
const allowedViteConfigProperties = new Set([
  "base",
  "plugins",
  "resolve",
  "build",
  "server",
]);
const allowedViteResolveProperties = new Set(["alias"]);
const allowedViteBuildProperties = new Set(["outDir", "target"]);
const allowedViteServerProperties = new Set(["allowedHosts", "proxy"]);
const viteInternalAliasSources = new Set([
  "^\\/?@vite\\/env",
  "^\\/?@vite\\/client",
]);

export class GuardError extends Error {}

function fail(message) {
  throw new GuardError(`${errorPrefix} ${message}`);
}

function relativePath(webRoot, filePath) {
  return pathRelative(webRoot, filePath).split(pathSep).join("/");
}

function isInside(root, candidate) {
  const relative = pathRelative(root, candidate);
  return relative === "" || (!relative.startsWith(`..${pathSep}`) && !pathIsAbsolute(relative));
}

function assertInsideWebRoot(candidate, webRoot, nodeModulesRoot, context, rejectNodeModules) {
  const resolved = pathResolve(candidate);
  if (!isInside(webRoot, resolved)) {
    fail(`${context} escapes the guarded web root: ${resolved}`);
  }
  if (rejectNodeModules && isInside(nodeModulesRoot, resolved)) {
    fail(`${context} reaches skipped node_modules: ${resolved}`);
  }
}

function assertOutsideSkippedDist(candidate, distRoot, context) {
  const resolved = pathResolve(candidate);
  if (isInside(distRoot, resolved)) {
    fail(`${context} reaches skipped web/dist: ${resolved}`);
  }
}

function assertSupportedResolvedFormat(filePath, context) {
  const extension = pathExtname(filePath).toLowerCase();
  if (
    scannedExtensions.has(extension) ||
    extension === ".html" ||
    inertSourceExtensions.has(extension)
  ) {
    return;
  }
  fail(`${context} resolves to an unrecognized source format: ${filePath}`);
}

function forbiddenSpecifier(specifier) {
  return (
    specifier === "react-router" ||
    specifier.startsWith("react-router/") ||
    specifier.startsWith("react-router-dom/") ||
    specifier.startsWith("@react-router/") ||
    specifier === "@vitejs/plugin-rsc" ||
    specifier.startsWith("react-server-dom-")
  );
}

function packageName(specifier) {
  if (specifier.startsWith("@")) {
    return specifier.split("/").slice(0, 2).join("/");
  }
  return specifier.split("/", 1)[0];
}

function isBuiltinSpecifier(specifier) {
  return specifier.startsWith("node:") || nodeBuiltins.has(specifier);
}

function isLocalSpecifier(specifier) {
  return specifier.startsWith(".") || specifier.startsWith("@/");
}

function literalText(node) {
  if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) {
    return node.text;
  }
  return null;
}

function importedName(node) {
  if (ts.isIdentifier(node) || ts.isStringLiteral(node)) {
    return node.text;
  }
  return null;
}

function scriptKind(filePath) {
  switch (pathExtname(filePath).toLowerCase()) {
    case ".tsx":
      return ts.ScriptKind.TSX;
    case ".jsx":
      return ts.ScriptKind.JSX;
    case ".ts":
    case ".mts":
    case ".cts":
      return ts.ScriptKind.TS;
    default:
      return ts.ScriptKind.JS;
  }
}

function parseSource(filePath, source) {
  const sourceFile = ts.createSourceFile(
    filePath,
    source,
    ts.ScriptTarget.Latest,
    true,
    scriptKind(filePath),
  );
  if (sourceFile.parseDiagnostics?.length) {
    fail(`${relativePath(pathDirname(filePath), filePath)} has a syntax error`);
  }
  return sourceFile;
}

function unwrapTimerHandler(node) {
  let current = node;
  while (
    current &&
    (ts.isParenthesizedExpression(current) ||
      ts.isAsExpression(current) ||
      ts.isTypeAssertionExpression(current) ||
      ts.isNonNullExpression(current) ||
      current.kind === ts.SyntaxKind.SatisfiesExpression)
  ) {
    current = current.expression;
  }
  return current;
}

function isServerEntry(filePath) {
  const basename = pathBasename(filePath);
  return (
    /^react-router\.config\./.test(basename) ||
    /^entry\.(?:server|rsc)\./.test(basename) ||
    /\.(?:server|rsc)\.[^.]+$/.test(basename)
  );
}

function moduleSpecifier(node, filePath, description) {
  const specifier = literalText(node);
  if (specifier === null) {
    fail(`${relativePath(pathDirname(filePath), filePath)} uses a nonliteral ${description}`);
  }
  return specifier;
}

function addModuleRecord(records, node, filePath, kind) {
  const specifier = moduleSpecifier(node, filePath, `${kind} module specifier`);
  const relative = relativePath(pathDirname(filePath), filePath);
  if (forbiddenSpecifier(specifier)) {
    fail(`${relative} imports RSC/server-capable module ${specifier}`);
  }
  records.push({ filePath, specifier, kind });
}

function inspectSource(filePath, source, records) {
  const sourceFile = parseSource(filePath, source);
  const relative = pathBasename(filePath);

  function isScopeNode(node) {
    return (
      ts.isSourceFile(node) ||
      ts.isBlock(node) ||
      ts.isModuleBlock(node) ||
      ts.isClassLike(node) ||
      ts.isFunctionLike(node) ||
      ts.isForStatement(node) ||
      ts.isForInStatement(node) ||
      ts.isForOfStatement(node) ||
      ts.isCatchClause(node)
    );
  }

  function nearestScope(node) {
    let current = node;
    while (current && !isScopeNode(current)) {
      current = current.parent;
    }
    return current ?? sourceFile;
  }

  function outerScope(scope) {
    return scope === sourceFile ? null : nearestScope(scope.parent);
  }

  const bindingsByScope = new Map();
  const promiseResolverCandidates = [];

  function addBinding(scope, name, callable) {
    if (!name) {
      return;
    }
    let bindings = bindingsByScope.get(scope);
    if (!bindings) {
      bindings = new Map();
      bindingsByScope.set(scope, bindings);
    }
    if (bindings.has(name)) {
      const binding = bindings.get(name);
      binding.callable &&= callable;
      return binding;
    }
    const binding = { callable, mutated: false };
    bindings.set(name, binding);
    return binding;
  }

  function addBindingNames(scope, name, callable = false) {
    if (ts.isIdentifier(name)) {
      return [addBinding(scope, name.text, callable)];
    }
    const bindings = [];
    for (const element of name.elements) {
      if (!ts.isOmittedExpression(element)) {
        bindings.push(...addBindingNames(scope, element.name, callable));
      }
    }
    return bindings;
  }

  function resolveBindingFromScope(scope, name) {
    let current = scope;
    while (current) {
      const binding = bindingsByScope.get(current)?.get(name);
      if (binding) {
        return binding;
      }
      current = outerScope(current);
    }
    return null;
  }

  function variableScope(declaration) {
    const declarationList = declaration.parent;
    if (declarationList.flags & ts.NodeFlags.Var) {
      let current = declaration.parent;
      while (current && !ts.isSourceFile(current) && !ts.isFunctionLike(current)) {
        current = current.parent;
      }
      return current ?? sourceFile;
    }
    return nearestScope(declaration);
  }

  function isPromiseResolverParameter(parameter) {
    const functionLike = parameter.parent;
    if (
      !ts.isFunctionLike(functionLike) ||
      functionLike.parameters[0] !== parameter
    ) {
      return false;
    }
    let promiseConstructor = functionLike.parent;
    while (promiseConstructor && ts.isParenthesizedExpression(promiseConstructor)) {
      promiseConstructor = promiseConstructor.parent;
    }
    return (
      ts.isNewExpression(promiseConstructor) &&
      ts.isIdentifier(promiseConstructor.expression) &&
      promiseConstructor.expression.text === "Promise" &&
      promiseConstructor.arguments?.[0] &&
      unwrapTimerHandler(promiseConstructor.arguments[0]) === functionLike
    );
  }

  function collectBindings(node) {
    if (ts.isFunctionDeclaration(node) && node.name) {
      addBinding(nearestScope(node.parent), node.name.text, true);
    }
    if (ts.isFunctionExpression(node) && node.name) {
      addBinding(node, node.name.text, false);
    }
    if (ts.isClassDeclaration(node) && node.name) {
      addBinding(nearestScope(node.parent), node.name.text, false);
    }
    if (ts.isClassExpression(node) && node.name) {
      addBinding(node, node.name.text, false);
    }
    if (ts.isImportClause(node)) {
      if (node.name) {
        addBinding(sourceFile, node.name.text, false);
      }
      const named = node.namedBindings;
      if (named && ts.isNamespaceImport(named)) {
        addBinding(sourceFile, named.name.text, false);
      } else if (named && ts.isNamedImports(named)) {
        for (const element of named.elements) {
          addBinding(sourceFile, element.name.text, false);
        }
      }
    }
    if (ts.isParameter(node)) {
      const bindings = addBindingNames(node.parent, node.name);
      if (isPromiseResolverParameter(node)) {
        promiseResolverCandidates.push({ binding: bindings[0], functionLike: node.parent });
      }
    }
    if (ts.isVariableDeclaration(node)) {
      const initializer = node.initializer && unwrapTimerHandler(node.initializer);
      const declarationList = node.parent;
      addBindingNames(
        variableScope(node),
        node.name,
        ts.isIdentifier(node.name) &&
          Boolean(
            declarationList.flags & ts.NodeFlags.Const &&
              initializer &&
              (ts.isArrowFunction(initializer) || ts.isFunctionExpression(initializer)),
          ),
      );
    }
    ts.forEachChild(node, collectBindings);
  }

  function markAssignedBindings(node, scope) {
    const target = unwrapTimerHandler(node);
    if (ts.isIdentifier(target)) {
      const binding = resolveBindingFromScope(scope, target.text);
      if (binding) {
        binding.mutated = true;
      }
      return;
    }
    if (ts.isArrayLiteralExpression(target)) {
      for (const element of target.elements) {
        if (!ts.isOmittedExpression(element)) {
          markAssignedBindings(element, scope);
        }
      }
      return;
    }
    if (ts.isObjectLiteralExpression(target)) {
      for (const property of target.properties) {
        if (ts.isShorthandPropertyAssignment(property)) {
          markAssignedBindings(property.name, scope);
        } else if (ts.isPropertyAssignment(property)) {
          markAssignedBindings(property.initializer, scope);
        } else if (ts.isSpreadAssignment(property)) {
          markAssignedBindings(property.expression, scope);
        }
      }
      return;
    }
    if (ts.isSpreadElement(target)) {
      markAssignedBindings(target.expression, scope);
    }
  }

  function collectAssignedNames(node) {
    if (ts.isIdentifier(node)) {
      const binding = resolveBindingFromScope(nearestScope(node.parent), node.text);
      if (binding) {
        binding.mutated = true;
      }
      return;
    }
    markAssignedBindings(node, nearestScope(node.parent));
  }

  function collectMutations(node) {
    if (
      ts.isBinaryExpression(node) &&
      node.operatorToken.kind >= ts.SyntaxKind.FirstAssignment &&
      node.operatorToken.kind <= ts.SyntaxKind.LastAssignment
    ) {
      collectAssignedNames(node.left);
    } else if (
      (ts.isForInStatement(node) || ts.isForOfStatement(node)) &&
      !ts.isVariableDeclarationList(node.initializer)
    ) {
      markAssignedBindings(node.initializer, nearestScope(node.initializer));
    } else if (
      (ts.isPrefixUnaryExpression(node) || ts.isPostfixUnaryExpression(node)) &&
      (node.operator === ts.SyntaxKind.PlusPlusToken ||
        node.operator === ts.SyntaxKind.MinusMinusToken)
    ) {
      collectAssignedNames(node.operand);
    }
    ts.forEachChild(node, collectMutations);
  }

  function isCallableIdentifier(node) {
    let scope = nearestScope(node.parent);
    while (scope) {
      const bindings = bindingsByScope.get(scope);
      if (bindings?.has(node.text)) {
        const binding = bindings.get(node.text);
        return binding.callable && !binding.mutated;
      }
      scope = outerScope(scope);
    }
    return false;
  }

  function isCallableTimerHandler(node) {
    const handler = unwrapTimerHandler(node);
    return (
      ts.isArrowFunction(handler) ||
      ts.isFunctionExpression(handler) ||
      (ts.isIdentifier(handler) && isCallableIdentifier(handler))
    );
  }

  function timerName(expression) {
    const timerExpression = unwrapTimerHandler(expression);
    if (
      ts.isIdentifier(timerExpression) &&
      timerNames.has(timerExpression.text) &&
      !resolveBindingFromScope(nearestScope(timerExpression.parent), timerExpression.text)
    ) {
      return timerExpression.text;
    }
    if (
      ts.isPropertyAccessExpression(timerExpression) &&
      ts.isIdentifier(timerExpression.expression) &&
      globalObjectNames.has(timerExpression.expression.text) &&
      !resolveBindingFromScope(
        nearestScope(timerExpression.expression.parent),
        timerExpression.expression.text,
      ) &&
      timerNames.has(timerExpression.name.text)
    ) {
      return timerExpression.name.text;
    }
    return null;
  }

  function isInsideTypeNode(node) {
    let current = node.parent;
    while (current && current !== sourceFile) {
      if (ts.isTypeNode(current)) {
        return true;
      }
      current = current.parent;
    }
    return false;
  }

  function isDirectCallCallee(node) {
    let current = node;
    while (
      current.parent &&
      (ts.isParenthesizedExpression(current.parent) ||
        ts.isAsExpression(current.parent) ||
        ts.isTypeAssertionExpression(current.parent) ||
        ts.isNonNullExpression(current.parent) ||
        current.parent.kind === ts.SyntaxKind.SatisfiesExpression)
    ) {
      current = current.parent;
    }
    return ts.isCallExpression(current.parent) && current.parent.expression === current;
  }

  function isUnshadowedGlobalIdentifier(node) {
    return (
      ts.isIdentifier(node) &&
      globalObjectNames.has(node.text) &&
      !resolveBindingFromScope(nearestScope(node.parent), node.text)
    );
  }

  function isReflectiveGlobalAccess(node) {
    return (
      ts.isCallExpression(node) &&
      ts.isPropertyAccessExpression(node.expression) &&
      ts.isIdentifier(node.expression.expression) &&
      node.expression.expression.text === "Reflect" &&
      !resolveBindingFromScope(nearestScope(node.expression.parent), "Reflect") &&
      node.arguments[0] &&
      isUnshadowedGlobalIdentifier(unwrapTimerHandler(node.arguments[0]))
    );
  }

  function objectGlobalPropertyAccess(node) {
    if (
      !ts.isCallExpression(node) ||
      !ts.isPropertyAccessExpression(node.expression) ||
      !ts.isIdentifier(node.expression.expression) ||
      node.expression.expression.text !== "Object" ||
      resolveBindingFromScope(nearestScope(node.expression.parent), "Object") ||
      !node.arguments[0] ||
      !isUnshadowedGlobalIdentifier(unwrapTimerHandler(node.arguments[0]))
    ) {
      return null;
    }
    return node.expression.name.text;
  }

  function isPropertyNameIdentifier(node) {
    const parent = node.parent;
    if (
      (ts.isImportSpecifier(parent) ||
        ts.isExportSpecifier(parent) ||
        ts.isBindingElement(parent)) &&
      parent.propertyName === node
    ) {
      return true;
    }
    return (
      (ts.isMethodDeclaration(parent) ||
        ts.isPropertyAccessExpression(parent) ||
        ts.isPropertyDeclaration(parent) ||
        ts.isGetAccessorDeclaration(parent) ||
        ts.isSetAccessorDeclaration(parent) ||
        ts.isMethodSignature(parent) ||
        ts.isPropertySignature(parent) ||
        ts.isPropertyAssignment(parent) ||
        ts.isJsxAttribute(parent)) &&
      parent.name === node
    );
  }

  function isAllowedGlobalObjectUse(node) {
    let current = node;
    while (
      current.parent &&
      (ts.isParenthesizedExpression(current.parent) ||
        ts.isAsExpression(current.parent) ||
        ts.isTypeAssertionExpression(current.parent) ||
        ts.isNonNullExpression(current.parent) ||
        current.parent.kind === ts.SyntaxKind.SatisfiesExpression)
    ) {
      current = current.parent;
    }
    const parent = current.parent;
    if (
      (ts.isPropertyAccessExpression(parent) || ts.isElementAccessExpression(parent)) &&
      parent.expression === current
    ) {
      return true;
    }
    if (ts.isTypeOfExpression(parent)) {
      return true;
    }
    if (
      ts.isBinaryExpression(parent) &&
      parent.operatorToken.kind === ts.SyntaxKind.InKeyword &&
      parent.right === current
    ) {
      return true;
    }
    if (ts.isCallExpression(parent) && parent.arguments[0] === current) {
      const access = objectGlobalPropertyAccess(parent);
      return access === "defineProperty" && isSafeWindowDataDescriptor(parent);
    }
    return false;
  }

  function isSafeWindowDataDescriptor(node) {
    if (
      literalText(node.arguments[1]) !== "window" ||
      !node.arguments[2] ||
      !ts.isObjectLiteralExpression(node.arguments[2])
    ) {
      return false;
    }
    const allowedKeys = new Set(["value", "configurable", "enumerable", "writable"]);
    return node.arguments[2].properties.every(
      (property) =>
        ts.isPropertyAssignment(property) &&
        ts.isIdentifier(property.name) &&
        allowedKeys.has(property.name.text),
    );
  }

  function verifyTimerHandler(node, name) {
    if (!node.arguments[0] || !isCallableTimerHandler(node.arguments[0])) {
      fail(`${relative} ${name} handler is not syntactically proven callable`);
    }
  }

  collectBindings(sourceFile);
  for (const { binding, functionLike } of promiseResolverCandidates) {
    const scope = nearestScope(functionLike.parent);
    if (binding && !resolveBindingFromScope(scope, "Promise")) {
      binding.callable = true;
    }
  }
  collectMutations(sourceFile);

  function visit(node) {
    if (ts.isIdentifier(node)) {
      if (forbiddenCodeGenerationIdentifiers.has(node.text)) {
        fail(`${relative} contains a forbidden code-generation identifier: ${node.text}`);
      }
      if (forbiddenRscIdentifiers.has(node.text)) {
        fail(`${relative} contains an unstable RSC API identifier: ${node.text}`);
      }
      if (
        timerNames.has(node.text) &&
        !resolveBindingFromScope(nearestScope(node.parent), node.text) &&
        !isInsideTypeNode(node) &&
        !isPropertyNameIdentifier(node) &&
        !isDirectCallCallee(node)
      ) {
        fail(`${relative} uses an indirect global timer reference: ${node.text}`);
      }
      if (
        isUnshadowedGlobalIdentifier(node) &&
        !isInsideTypeNode(node) &&
        !isPropertyNameIdentifier(node) &&
        !isAllowedGlobalObjectUse(node)
      ) {
        fail(`${relative} uses an indirect global object reference: ${node.text}`);
      }
    }

    if (
      ts.isPropertyAccessExpression(node) &&
      timerName(node) &&
      !isInsideTypeNode(node) &&
      !isDirectCallCallee(node)
    ) {
      fail(`${relative} uses an indirect global timer reference: ${node.name.text}`);
    }

    if (
      ts.isPropertyAccessExpression(node) &&
      isUnshadowedGlobalIdentifier(node.expression) &&
      node.name.text === "Promise"
    ) {
      fail(`${relative} accesses replaceable global Promise state`);
    }

    if (isReflectiveGlobalAccess(node)) {
      fail(`${relative} uses forbidden reflective global access`);
    }

    const objectGlobalAccess = objectGlobalPropertyAccess(node);
    if (objectGlobalAccess) {
      if (objectGlobalAccess === "defineProperty") {
        if (!isSafeWindowDataDescriptor(node)) {
          fail(`${relative} uses forbidden Object global property access`);
        }
      } else if (
        [
          "assign",
          "defineProperties",
          "getOwnPropertyDescriptor",
          "getOwnPropertyDescriptors",
        ].includes(objectGlobalAccess)
      ) {
        fail(`${relative} uses forbidden Object global property access`);
      }
    }

    if (
      ts.isElementAccessExpression(node) &&
      isUnshadowedGlobalIdentifier(node.expression)
    ) {
      const key = ts.isStringLiteralLike(node.argumentExpression)
        ? node.argumentExpression.text
        : "computed property";
      fail(`${relative} uses forbidden computed global access: ${key}`);
    }

    if (
      ts.isExpressionStatement(node) &&
      (ts.isStringLiteral(node.expression) ||
        ts.isNoSubstitutionTemplateLiteral(node.expression)) &&
      (node.expression.text === "use server" || node.expression.text === "react-server")
    ) {
      fail(`${relative} contains a server directive`);
    }

    if (ts.isImportDeclaration(node)) {
      addModuleRecord(records, node.moduleSpecifier, filePath, "static import");
      const specifier = literalText(node.moduleSpecifier);
      const bindings = node.importClause?.namedBindings;
      if (
        bindings &&
        ts.isNamespaceImport(bindings) &&
        specifier === "react-router-dom"
      ) {
        fail(`${relative} uses a namespace import from react-router-dom`);
      }
      if (specifier === "react-router-dom" && bindings && ts.isNamedImports(bindings)) {
        for (const element of bindings.elements) {
          if (node.importClause?.isTypeOnly || element.isTypeOnly) {
            continue;
          }
          const name = importedName(element.propertyName ?? element.name);
          if (!name || !allowedReactRouterDomValueExports.has(name)) {
            fail(`${relative} imports an unapproved React Router value export: ${name ?? "unknown"}`);
          }
        }
      }
      if (specifier === "react-router-dom" && node.importClause?.name) {
        fail(`${relative} uses a default import from react-router-dom`);
      }
    }

    if (ts.isExportDeclaration(node) && node.moduleSpecifier) {
      const specifier = moduleSpecifier(node.moduleSpecifier, filePath, "export module specifier");
      if (
        specifier === "react-router-dom" &&
        (!node.exportClause || ts.isNamespaceExport(node.exportClause))
      ) {
        fail(`${relative} re-exports the full react-router-dom namespace`);
      }
      if (
        specifier === "react-router-dom" &&
        node.exportClause &&
        ts.isNamedExports(node.exportClause)
      ) {
        for (const element of node.exportClause.elements) {
          if (node.isTypeOnly || element.isTypeOnly) {
            continue;
          }
          const name = importedName(element.propertyName ?? element.name);
          if (!name || !allowedReactRouterDomValueExports.has(name)) {
            fail(`${relative} re-exports an unapproved React Router value export: ${name ?? "unknown"}`);
          }
        }
      }
      addModuleRecord(records, node.moduleSpecifier, filePath, "export");
    }

    if (ts.isImportEqualsDeclaration(node) && ts.isExternalModuleReference(node.moduleReference)) {
      addModuleRecord(records, node.moduleReference.expression, filePath, "import-equals");
    }

    if (ts.isImportTypeNode(node)) {
      const argument = ts.isLiteralTypeNode(node.argument)
        ? node.argument.literal
        : node.argument;
      addModuleRecord(records, argument, filePath, "import type");
    }

    if (ts.isCallExpression(node)) {
      const timer = timerName(node.expression);
      if (timer) {
        verifyTimerHandler(node, timer);
      }
      const isDynamicImport = node.expression.kind === ts.SyntaxKind.ImportKeyword;
      const isRequire = ts.isIdentifier(node.expression) && node.expression.text === "require";
      if (isDynamicImport || isRequire) {
        if (node.arguments.length !== 1) {
          fail(`${relative} uses a nonliteral dynamic module specifier`);
        }
        const argument = node.arguments[0];
        const specifier = literalText(argument);
        if (specifier === null) {
          fail(`${relative} uses a nonliteral dynamic module specifier`);
        }
        if (specifier === "react-router-dom") {
          fail(`${relative} dynamically imports react-router-dom`);
        }
        addModuleRecord(
          records,
          argument,
          filePath,
          isDynamicImport ? "dynamic import" : "require",
        );
      }
    }

    ts.forEachChild(node, visit);
  }

  visit(sourceFile);
}

function parseScriptAttributes(filePath, attributeText) {
  const attributes = [];
  let index = 0;
  while (index < attributeText.length) {
    while (/\s/.test(attributeText[index] ?? "")) {
      index += 1;
    }
    if (index >= attributeText.length) {
      break;
    }
    if (attributeText[index] === "/" && /^[\s/]*$/.test(attributeText.slice(index))) {
      break;
    }

    const name = attributeText.slice(index).match(/^[A-Za-z_:][A-Za-z0-9_.:-]*/)?.[0];
    if (!name) {
      fail(`${relativePath(pathDirname(filePath), filePath)} has malformed script tag attributes`);
    }
    index += name.length;
    while (/\s/.test(attributeText[index] ?? "")) {
      index += 1;
    }

    let value = null;
    if (attributeText[index] === "=") {
      index += 1;
      while (/\s/.test(attributeText[index] ?? "")) {
        index += 1;
      }
      const quote = attributeText[index];
      if (quote === '"' || quote === "'") {
        index += 1;
        const end = attributeText.indexOf(quote, index);
        if (end === -1) {
          fail(`${relativePath(pathDirname(filePath), filePath)} has malformed script src`);
        }
        value = attributeText.slice(index, end);
        index = end + 1;
      } else {
        const start = index;
        while (index < attributeText.length && !/\s/.test(attributeText[index])) {
          index += 1;
        }
        value = attributeText.slice(start, index);
        if (name.toLowerCase() === "src") {
          fail(`${relativePath(pathDirname(filePath), filePath)} has an unquoted script src`);
        }
      }
    }
    attributes.push({ name: name.toLowerCase(), value });
  }
  return attributes;
}

function verifyHtmlScriptTarget(filePath, src, webRoot, nodeModulesRoot, distRoot) {
  if (/[&\s]/.test(src)) {
    fail(`${relativePath(webRoot, filePath)} has an ambiguous script src`);
  }
  const rawPath = src.split(/[?#]/, 1)[0];
  if (!rawPath || rawPath.includes("\\")) {
    fail(`${relativePath(webRoot, filePath)} has an invalid local script src`);
  }
  let decodedPath;
  try {
    decodedPath = decodeURIComponent(rawPath);
  } catch {
    fail(`${relativePath(webRoot, filePath)} has malformed script src encoding`);
  }
  if (
    decodedPath.startsWith("//") ||
    /^[A-Za-z][A-Za-z0-9+.-]*:/.test(decodedPath)
  ) {
    fail(`${relativePath(webRoot, filePath)} has an external script src`);
  }
  const candidate = decodedPath.startsWith("/")
    ? pathResolve(webRoot, decodedPath.slice(1))
    : pathResolve(pathDirname(filePath), decodedPath);
  assertInsideWebRoot(candidate, webRoot, nodeModulesRoot, "HTML script src", true);
  assertOutsideSkippedDist(candidate, distRoot, "HTML script src");
  if (!fsExistsSync(candidate)) {
    fail(`${relativePath(webRoot, filePath)} references a missing script src: ${src}`);
  }
  const canonical = fsRealpathSync(candidate);
  assertInsideWebRoot(canonical, webRoot, nodeModulesRoot, "HTML script src", true);
  assertOutsideSkippedDist(canonical, distRoot, "HTML script src");
  if (!fsStatSync(canonical).isFile()) {
    fail(`${relativePath(webRoot, filePath)} references a non-file script src: ${src}`);
  }
  assertSupportedResolvedFormat(canonical, `${relativePath(webRoot, filePath)} script src ${src}`);
}

function inspectHtml(filePath, source, records, webRoot, nodeModulesRoot, distRoot) {
  const markupSource = source.replace(/<!--[\s\S]*?-->/g, (comment) => " ".repeat(comment.length));
  const openingPattern = /<script\b/gi;
  let markupStart = 0;
  while (true) {
    const opening = openingPattern.exec(markupSource);
    if (!opening) {
      if (/<base(?=[\s/>])/i.test(markupSource.slice(markupStart))) {
        fail(`${relativePath(webRoot, filePath)} contains a base element`);
      }
      break;
    }
    if (/<base(?=[\s/>])/i.test(markupSource.slice(markupStart, opening.index))) {
      fail(`${relativePath(webRoot, filePath)} contains a base element`);
    }
    let tagEnd = opening.index + opening[0].length;
    let quote = null;
    for (; tagEnd < markupSource.length; tagEnd += 1) {
      const character = markupSource[tagEnd];
      if (quote) {
        if (character === quote) {
          quote = null;
        }
      } else if (character === '"' || character === "'") {
        quote = character;
      } else if (character === ">") {
        break;
      }
    }
    if (tagEnd >= markupSource.length || quote) {
      fail(`${relativePath(webRoot, filePath)} has a malformed script tag`);
    }
    const attributes = parseScriptAttributes(
      filePath,
      markupSource.slice(opening.index + opening[0].length, tagEnd),
    );
    const srcAttributes = attributes.filter(({ name }) => name === "src");
    if (srcAttributes.length > 1) {
      fail(`${relativePath(webRoot, filePath)} has multiple script src attributes`);
    }
    if (srcAttributes.length === 1) {
      if (!srcAttributes[0].value) {
        fail(`${relativePath(webRoot, filePath)} has a malformed script src`);
      }
      verifyHtmlScriptTarget(
        filePath,
        srcAttributes[0].value,
        webRoot,
        nodeModulesRoot,
        distRoot,
      );
    }

    const closePattern = /<\/script\s*>/gi;
    closePattern.lastIndex = tagEnd + 1;
    const closing = closePattern.exec(markupSource);
    if (!closing) {
      fail(`${relativePath(webRoot, filePath)} has an unclosed script tag`);
    }
    inspectSource(filePath, source.slice(tagEnd + 1, closing.index), records);
    openingPattern.lastIndex = closing.index + closing[0].length;
    markupStart = openingPattern.lastIndex;
  }
}

function collectSourceFiles(webRoot, webSourceRoot) {
  const files = [];
  const ignoredPaths = new Set([
    pathJoin(webRoot, "dist"),
    pathJoin(webRoot, "node_modules"),
  ]);

  function walk(directory) {
    const entries = fsReaddirSync(directory, { withFileTypes: true });
    entries.sort((left, right) => left.name.localeCompare(right.name));
    for (const entry of entries) {
      const entryPath = pathJoin(directory, entry.name);
      if (entry.isSymbolicLink()) {
        fail(`${relativePath(webRoot, entryPath)} is a symbolic link`);
      }
      if (entry.isDirectory()) {
        if (ignoredPaths.has(entryPath)) {
          continue;
        }
        walk(entryPath);
        continue;
      }
      if (entry.isFile() && scannedExtensions.has(pathExtname(entry.name).toLowerCase())) {
        files.push(entryPath);
      } else if (
        entry.isFile() &&
        unsupportedExecutableExtensions.has(pathExtname(entry.name).toLowerCase())
      ) {
        fail(`${relativePath(webRoot, entryPath)} uses an unsupported executable source format`);
      } else if (entry.isFile() && pathExtname(entry.name).toLowerCase() === ".html") {
        files.push(entryPath);
      } else if (
        entry.isFile() &&
        isInside(webSourceRoot, entryPath) &&
        !inertSourceExtensions.has(pathExtname(entry.name).toLowerCase())
      ) {
        fail(`${relativePath(webRoot, entryPath)} uses an unrecognized source format`);
      }
    }
  }

  walk(webRoot);
  return files;
}

function declaredPackageNames(packagePath) {
  if (!fsExistsSync(packagePath)) {
    fail("missing web/package.json");
  }
  const packageJson = JSON.parse(fsReadFileSync(packagePath, "utf8"));
  if (!packageJson.dependencies?.["react-router-dom"]) {
    fail("react-router-dom must remain a direct runtime dependency");
  }
  const declared = new Set();
  for (const section of [
    "dependencies",
    "devDependencies",
    "optionalDependencies",
    "peerDependencies",
  ]) {
    for (const name of Object.keys(packageJson[section] ?? {})) {
      declared.add(name);
    }
  }
  return declared;
}

function captureGuardedInputSnapshot(packagePath, configFile) {
  return new Map([
    [packagePath, fsReadFileSync(packagePath, "utf8")],
    [configFile, fsReadFileSync(configFile, "utf8")],
  ]);
}

function verifyGuardedInputSnapshot(
  snapshot,
  packagePath,
  configFile,
  webRoot,
  webSourceRoot,
  phase,
) {
  const currentPaths = new Set(collectSourceFiles(webRoot, webSourceRoot));
  if (fsExistsSync(packagePath)) {
    currentPaths.add(packagePath);
  }
  if (fsExistsSync(configFile)) {
    currentPaths.add(configFile);
  }
  for (const filePath of snapshot.keys()) {
    if (!setHas(currentPaths, filePath)) {
      fail(
        `guarded input snapshot changed after ${phase}: removed ${relativePath(webRoot, filePath)}`,
      );
    }
  }
  for (const filePath of currentPaths) {
    if (!mapHas(snapshot, filePath)) {
      fail(
        `guarded input snapshot changed after ${phase}: added ${relativePath(webRoot, filePath)}`,
      );
    }
  }
  for (const [filePath, content] of snapshot) {
    let currentContent;
    try {
      currentContent = fsReadFileSync(filePath, "utf8");
    } catch {
      fail(
        `guarded input snapshot changed after ${phase}: modified ${relativePath(webRoot, filePath)}`,
      );
    }
    if (currentContent !== content) {
      fail(
        `guarded input snapshot changed after ${phase}: modified ${relativePath(webRoot, filePath)}`,
      );
    }
  }
}

function rawResolvedId(id) {
  return typeof id === "string" ? id : id?.id;
}

function resolvedIdPath(rawId) {
  const withoutQuery = rawId.split(/[?#]/, 1)[0];
  if (withoutQuery.startsWith("file://")) {
    return fileURLToPath(withoutQuery);
  }
  return pathResolve(withoutQuery);
}

function aliasEntries(alias) {
  if (arrayIsArray(alias)) {
    return alias;
  }
  if (alias && typeof alias === "object") {
    return objectEntries(alias).map(([find, replacement]) => ({ find, replacement }));
  }
  return [];
}

function isPlainObject(value) {
  if (value === null || typeof value !== "object" || arrayIsArray(value)) {
    return false;
  }
  const prototype = objectGetPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}

function enumerableOwnKeys(value, location) {
  const keys = [];
  for (const key in value) {
    if (!objectHasOwn(value, key)) {
      fail(`Vite configuration contains an inherited property at ${location || "root"}: ${key}`);
    }
    keys.push(key);
  }
  return keys;
}

function isViteInternalAlias(entry) {
  const normalizedReplacement =
    typeof entry?.replacement === "string"
      ? entry.replacement.replaceAll("\\", "/")
      : null;
  return (
    entry?.find instanceof RegExp &&
    entry.find.flags === "" &&
    setHas(viteInternalAliasSources, entry.find.source) &&
    typeof entry.replacement === "string" &&
    normalizedReplacement.includes("/vite/dist/client/")
  );
}

function aliasMatchesSpecifier(entry, specifier) {
  if (!isPlainObject(entry)) {
    return false;
  }
  if (typeof entry.find === "string") {
    return specifier === entry.find || specifier.startsWith(`${entry.find}/`);
  }
  if (entry.find instanceof RegExp) {
    entry.find.lastIndex = 0;
    return entry.find.test(specifier);
  }
  return false;
}

function hasNonApprovedAliasForSpecifier(server, specifier) {
  return aliasEntries(server.config.resolve?.alias).some(
    (entry) =>
      isPlainObject(entry) &&
      !isViteInternalAlias(entry) &&
      entry.find !== "@" &&
      aliasMatchesSpecifier(entry, specifier),
  );
}

function verifyAliasEntries(
  alias,
  webRoot,
  webSourceRoot,
  nodeModulesRoot,
  context,
) {
  const entries = aliasEntries(alias);
  if (
    entries.length !== 1 ||
    !isPlainObject(entries[0]) ||
    entries[0].find !== "@"
  ) {
    fail(`${context} configuration must contain exactly one @ alias`);
  }
  const entry = entries[0];
  if (
    enumerableOwnKeys(entry, `${context} @ alias`).some(
      (key) => !["find", "replacement"].includes(key),
    )
  ) {
    fail(`${context} @ alias has unsupported properties`);
  }
  if (typeof entry.replacement !== "string") {
    fail(`${context} @ alias must have a string replacement`);
  }
  const aliasRoot = pathResolve(webRoot, entry.replacement);
  const checkedAliasRoot = fsExistsSync(aliasRoot) ? fsRealpathSync(aliasRoot) : aliasRoot;
  assertInsideWebRoot(
    checkedAliasRoot,
    webRoot,
    nodeModulesRoot,
    `${context} @ alias`,
    true,
  );
  if (checkedAliasRoot !== webSourceRoot) {
    fail(`${context} @ alias must be rooted at web/src`);
  }
}

function verifyEffectiveAlias(server, webRoot, webSourceRoot, nodeModulesRoot) {
  // Vite adds its two client aliases after loading user config; the raw user
  // configuration is checked separately below, so only these known aliases are exempt.
  const entries = aliasEntries(server.config.resolve?.alias).filter(
    (entry) => !isViteInternalAlias(entry),
  );
  verifyAliasEntries(
    entries,
    webRoot,
    webSourceRoot,
    nodeModulesRoot,
    "effective Vite",
  );
}

function verifyAllowedViteProperties(value, allowed, location) {
  if (!isPlainObject(value)) {
    fail(`Vite configuration ${location} must be a plain object`);
  }
  for (const key of enumerableOwnKeys(value, location)) {
    if (!setHas(allowed, key)) {
      fail(`Vite configuration ${location} contains an unapproved property: ${key}`);
    }
  }
}

function verifyViteConfigShape(config, webRoot, webSourceRoot, nodeModulesRoot) {
  if (!isPlainObject(config)) {
    fail("Vite configuration must be a plain object");
  }
  verifyNoNestedVitePluginOptions(config);
  for (const key of enumerableOwnKeys(config, "root")) {
    if (!setHas(allowedViteConfigProperties, key)) {
      fail(`Vite configuration contains an unapproved top-level property: ${key}`);
    }
  }
  if (config.base !== undefined && typeof config.base !== "string") {
    fail("Vite configuration base must be a string");
  }
  if (config.resolve !== undefined) {
    verifyAllowedViteProperties(config.resolve, allowedViteResolveProperties, "resolve");
    verifyAliasEntries(
      config.resolve.alias,
      webRoot,
      webSourceRoot,
      nodeModulesRoot,
      "user Vite",
    );
  }
  if (config.build !== undefined) {
    verifyAllowedViteProperties(config.build, allowedViteBuildProperties, "build");
    if (config.build.outDir !== undefined && typeof config.build.outDir !== "string") {
      fail("Vite configuration build.outDir must be a string");
    }
    const target = config.build.target;
    if (
      target !== undefined &&
      typeof target !== "string" &&
      (!arrayIsArray(target) || target.some((entry) => typeof entry !== "string"))
    ) {
      fail("Vite configuration build.target must be a string or string array");
    }
  }
  if (config.server !== undefined) {
    verifyAllowedViteProperties(config.server, allowedViteServerProperties, "server");
  }
}

async function verifyImportBoundary(
  server,
  record,
  webRoot,
  webSourceRoot,
  nodeModulesRoot,
  distRoot,
  declared,
) {
  const packageNameValue = packageName(record.specifier);
  const local = isLocalSpecifier(record.specifier);
  if (
    !local &&
    !isBuiltinSpecifier(record.specifier) &&
    !setHas(declared, packageNameValue) &&
    !setHas(allowedTransitiveImports, packageNameValue)
  ) {
    fail(`${relativePath(webRoot, record.filePath)} imports undeclared package or local alias ${record.specifier}`);
  }
  if (isBuiltinSpecifier(record.specifier)) {
    return;
  }

  const resolved = await server.pluginContainer.resolveId(record.specifier, record.filePath);
  const rawId = rawResolvedId(resolved);
  if (local) {
    if (!rawId) {
      const lexicalPath = record.specifier.startsWith("@/")
        ? pathResolve(webSourceRoot, record.specifier.slice(2))
        : pathResolve(pathDirname(record.filePath), record.specifier);
      assertInsideWebRoot(
        lexicalPath,
        webRoot,
        nodeModulesRoot,
        `${relativePath(webRoot, record.filePath)} unresolved import ${record.specifier}`,
        true,
      );
      assertOutsideSkippedDist(
        lexicalPath,
        distRoot,
        `${relativePath(webRoot, record.filePath)} unresolved import ${record.specifier}`,
      );
      return;
    }
    if (rawId.startsWith("\0")) {
      fail(`${relativePath(webRoot, record.filePath)} resolves to a virtual module: ${record.specifier}`);
    }
    const resolvedPath = resolvedIdPath(rawId);
    assertOutsideSkippedDist(
      resolvedPath,
      distRoot,
      `${relativePath(webRoot, record.filePath)} import ${record.specifier}`,
    );
    const canonicalPath = fsExistsSync(resolvedPath)
      ? fsRealpathSync(resolvedPath)
      : resolvedPath;
    assertInsideWebRoot(
      canonicalPath,
      webRoot,
      nodeModulesRoot,
      `${relativePath(webRoot, record.filePath)} import ${record.specifier}`,
      true,
    );
    assertOutsideSkippedDist(
      canonicalPath,
      distRoot,
      `${relativePath(webRoot, record.filePath)} import ${record.specifier}`,
    );
    assertSupportedResolvedFormat(
      resolvedPath,
      `${relativePath(webRoot, record.filePath)} import ${record.specifier}`,
    );
    return;
  }

  if (!rawId) {
    fail(`${relativePath(webRoot, record.filePath)} cannot resolve package import ${record.specifier}`);
  }
  if (rawId.startsWith("\0")) {
    fail(`${relativePath(webRoot, record.filePath)} resolves to a virtual module: ${record.specifier}`);
  }
  const resolvedPath = resolvedIdPath(rawId);
  const canonicalPath = fsExistsSync(resolvedPath)
    ? fsRealpathSync(resolvedPath)
    : resolvedPath;
  if (!isInside(webRoot, canonicalPath)) {
    fail(`${relativePath(webRoot, record.filePath)} resolves outside the guarded web root: ${record.specifier}`);
  }
  if (
    isInside(nodeModulesRoot, canonicalPath) &&
    hasNonApprovedAliasForSpecifier(server, record.specifier)
  ) {
    // Direct declared dependencies may resolve into node_modules; an alias
    // redirect there would hide an unchecked source module from this guard.
    fail(
      `${relativePath(webRoot, record.filePath)} bare package import ${record.specifier} is redirected into skipped node_modules: ${canonicalPath}`,
    );
  }
  assertOutsideSkippedDist(
    canonicalPath,
    distRoot,
    `${relativePath(webRoot, record.filePath)} import ${record.specifier}`,
  );
}

async function loadViteServer(webRoot, configFile) {
  return createServer({
    root: webRoot,
    configFile,
    appType: "custom",
    logLevel: "silent",
    server: { middlewareMode: true },
  });
}

function propertyName(node) {
  if (ts.isIdentifier(node) || ts.isStringLiteralLike(node)) {
    return node.text;
  }
  return null;
}

function memberChain(node) {
  if (ts.isIdentifier(node)) {
    return [node.text];
  }
  if (ts.isPropertyAccessExpression(node)) {
    const parent = memberChain(node.expression);
    return parent ? [...parent, node.name.text] : null;
  }
  if (
    ts.isElementAccessExpression(node) &&
    node.argumentExpression &&
    ts.isStringLiteralLike(node.argumentExpression)
  ) {
    const parent = memberChain(node.expression);
    return parent ? [...parent, node.argumentExpression.text] : null;
  }
  return null;
}

function importedBinding(sourceFile, moduleName, imported) {
  const matches = [];
  for (const statement of sourceFile.statements) {
    if (
      !ts.isImportDeclaration(statement) ||
      literalText(statement.moduleSpecifier) !== moduleName ||
      !statement.importClause
    ) {
      continue;
    }
    if (imported === "default" && statement.importClause.name) {
      matches.push(statement.importClause.name.text);
    }
    const bindings = statement.importClause.namedBindings;
    if (imported !== "default" && bindings && ts.isNamedImports(bindings)) {
      for (const element of bindings.elements) {
        if (importedName(element.propertyName ?? element.name) === imported) {
          matches.push(element.name.text);
        }
      }
    }
  }
  if (matches.length !== 1) {
    fail(`Vite config must import ${imported} exactly once from ${moduleName}`);
  }
  return matches[0];
}

function unwrapParentheses(node) {
  let current = node;
  while (ts.isParenthesizedExpression(current)) {
    current = current.expression;
  }
  return current;
}

function verifyViteConfigHasNoPrototypeMutation(configFile) {
  const sourceFile = parseSource(configFile, fsReadFileSync(configFile, "utf8"));
  const allowedImports = new Set([
    "vite",
    "@vitejs/plugin-react",
    "@tailwindcss/vite",
    "node:url",
    "path",
  ]);
  const intrinsicRoots = new Set([
    "Array",
    "Function",
    "Map",
    "Object",
    "Reflect",
    "Set",
    "WeakSet",
  ]);
  const allowedProcessEnv = new Set([
    "ZEROCLAW_GATEWAY_HOST",
    "ZEROCLAW_GATEWAY_PORT",
    "ZEROCLAW_WEB_ALLOWED_HOSTS",
  ]);
  const restrictedRuntimeRoots = new Set([
    ...intrinsicRoots,
    "global",
    "globalThis",
    "module",
    "process",
    "require",
  ]);
  const importedBindings = new Set();
  const aliases = new Map();

  function resolvedChain(node) {
    const raw = memberChain(node);
    if (!raw) {
      return null;
    }
    const chain = raw[0] === "globalThis" && setHas(intrinsicRoots, raw[1])
      ? raw.slice(1)
      : raw;
    const alias = aliases.get(chain[0]);
    return alias ? [...alias, ...chain.slice(1)] : chain;
  }

  for (const statement of sourceFile.statements) {
    if (ts.isImportDeclaration(statement)) {
      const moduleName = literalText(statement.moduleSpecifier);
      if (!statement.importClause || !moduleName || !setHas(allowedImports, moduleName)) {
        fail("Vite config cannot import side-effect or unapproved modules");
      }
      if (statement.importClause.name) {
        importedBindings.add(statement.importClause.name.text);
      }
      const bindings = statement.importClause.namedBindings;
      if (bindings && ts.isNamespaceImport(bindings)) {
        importedBindings.add(bindings.name.text);
      } else if (bindings && ts.isNamedImports(bindings)) {
        for (const element of bindings.elements) {
          importedBindings.add(element.name.text);
        }
      }
    }
  }

  function collectRestrictedAliases(node) {
    if (ts.isVariableDeclaration(node) && node.initializer) {
      const chain = resolvedChain(node.initializer);
      if (
        chain &&
        (setHas(intrinsicRoots, chain[0]) || setHas(importedBindings, chain[0]))
      ) {
        if (!ts.isIdentifier(node.name)) {
          fail("Vite config cannot destructure restricted bindings");
        }
        aliases.set(node.name.text, chain);
      }
    }
    ts.forEachChild(node, collectRestrictedAliases);
  }
  collectRestrictedAliases(sourceFile);

  function outerMemberChain(node) {
    let current = node;
    while (
      current.parent &&
      (ts.isPropertyAccessExpression(current.parent) ||
        ts.isElementAccessExpression(current.parent)) &&
      current.parent.expression === current
    ) {
      current = current.parent;
    }
    return memberChain(current);
  }

  function isApprovedRuntimeRootAccess(node) {
    const chain = outerMemberChain(node);
    if (node.text === "process") {
      return (
        chain?.length >= 3 &&
        chain[1] === "env" &&
        setHas(allowedProcessEnv, chain[2])
      );
    }
    return false;
  }

  function rejectPrototypeMutation(node) {
    if (
      ts.isIdentifier(node) &&
      setHas(restrictedRuntimeRoots, node.text) &&
      !isApprovedRuntimeRootAccess(node)
    ) {
      fail(
        "Vite config cannot use reflective or unrestricted runtime globals, mutable object prototypes, or prototype mutation primitives",
      );
    }
    const chain = resolvedChain(node);
    const parentUsesNodeAsReceiver = node.parent &&
      ((ts.isPropertyAccessExpression(node.parent) ||
        ts.isElementAccessExpression(node.parent)) &&
        node.parent.expression === node);
    const isImportDeclarationBinding =
      ts.isIdentifier(node) &&
      (ts.isImportClause(node.parent) ||
        ts.isImportSpecifier(node.parent) ||
        ts.isNamespaceImport(node.parent));
    const isDirectCallTarget = node.parent &&
      (ts.isCallExpression(node.parent) || ts.isNewExpression(node.parent)) &&
      node.parent.expression === node;
    if (
      !parentUsesNodeAsReceiver &&
      !isImportDeclarationBinding &&
      !isDirectCallTarget &&
      setHas(importedBindings, chain?.[0])
    ) {
      fail("Vite config cannot mutate imported modules through aliases or arguments");
    }
    if (chain?.includes("__proto__")) {
      fail("Vite config cannot use __proto__ properties");
    }
    if (chain?.includes("prototype")) {
      fail("Vite config cannot access mutable object prototypes");
    }
    if (chain?.includes("constructor")) {
      fail("Vite config cannot access dynamic code constructors");
    }
    if (
      ts.isElementAccessExpression(node) &&
      !ts.isStringLiteralLike(node.argumentExpression) &&
      setHas(intrinsicRoots, resolvedChain(node.expression)?.[0])
    ) {
      fail("Vite config cannot dynamically access mutable global intrinsics");
    }
    if (ts.isCallExpression(node)) {
      if (node.expression.kind === ts.SyntaxKind.ImportKeyword) {
        fail("Vite config cannot use dynamic imports");
      }
      const callee = resolvedChain(node.expression)?.join(".");
      if (
        [
          "fetch",
          "globalThis.fetch",
          "globalThis.queueMicrotask",
          "globalThis.setImmediate",
          "globalThis.setInterval",
          "globalThis.setTimeout",
          "process.getBuiltinModule",
          "process.nextTick",
          "queueMicrotask",
          "setImmediate",
          "setInterval",
          "setTimeout",
        ].includes(callee)
      ) {
        fail("Vite config cannot schedule or start external work");
      }
      if (
        [
          "Object.defineProperty",
          "Object.defineProperties",
          "Object.setPrototypeOf",
          "Reflect.deleteProperty",
          "Reflect.defineProperty",
          "Reflect.setPrototypeOf",
        ].includes(callee)
      ) {
        fail("Vite config cannot use prototype mutation primitives");
      }
      if (
        callee === "Object.assign" &&
        (setHas(intrinsicRoots, resolvedChain(node.arguments[0])?.[0]) ||
          setHas(importedBindings, resolvedChain(node.arguments[0])?.[0]))
      ) {
        fail("Vite config cannot mutate restricted bindings with Object.assign");
      }
      if (["eval", "Function", "globalThis.eval"].includes(callee)) {
        fail("Vite config cannot use dynamic code execution");
      }
    }
    const processChain =
      (ts.isPropertyAccessExpression(node) || ts.isElementAccessExpression(node)) &&
      !parentUsesNodeAsReceiver
        ? memberChain(node)
        : null;
    if (
      processChain?.[0] === "process" &&
      !(
        processChain.length >= 3 &&
        processChain[1] === "env" &&
        setHas(allowedProcessEnv, processChain[2])
      )
    ) {
      fail("Vite config can only read approved process environment fields");
    }
    if (
      ts.isBinaryExpression(node) &&
      node.operatorToken.kind >= ts.SyntaxKind.FirstAssignment &&
      node.operatorToken.kind <= ts.SyntaxKind.LastAssignment
    ) {
      const root = resolvedChain(node.left)?.[0];
      if (setHas(intrinsicRoots, root)) {
        fail("Vite config cannot reassign mutable global intrinsics");
      }
      if (setHas(importedBindings, root)) {
        fail("Vite config cannot mutate imported modules");
      }
      if (root === "process") {
        fail("Vite config cannot mutate process environment fields");
      }
    }
    if (
      ts.isDeleteExpression(node) &&
      setHas(intrinsicRoots, resolvedChain(node.expression)?.[0])
    ) {
      fail("Vite config cannot delete mutable global intrinsics");
    }
    if (
      ts.isDeleteExpression(node) &&
      setHas(importedBindings, resolvedChain(node.expression)?.[0])
    ) {
      fail("Vite config cannot mutate imported modules");
    }
    if (
      ts.isDeleteExpression(node) &&
      resolvedChain(node.expression)?.[0] === "process"
    ) {
      fail("Vite config cannot mutate process environment fields");
    }
    if (
      (ts.isPrefixUnaryExpression(node) || ts.isPostfixUnaryExpression(node)) &&
      (node.operator === ts.SyntaxKind.PlusPlusToken ||
        node.operator === ts.SyntaxKind.MinusMinusToken) &&
      resolvedChain(node.operand)?.[0] === "process"
    ) {
      fail("Vite config cannot mutate process environment fields");
    }
    ts.forEachChild(node, rejectPrototypeMutation);
  }
  rejectPrototypeMutation(sourceFile);
}

function verifyViteConfigPluginSource(configFile) {
  const sourceFile = parseSource(configFile, fsReadFileSync(configFile, "utf8"));
  const defineConfigName = importedBinding(sourceFile, "vite", "defineConfig");
  const reactName = importedBinding(sourceFile, "@vitejs/plugin-react", "default");
  const tailwindName = importedBinding(sourceFile, "@tailwindcss/vite", "default");
  const exports = sourceFile.statements.filter(
    (statement) => ts.isExportAssignment(statement) && !statement.isExportEquals,
  );
  if (exports.length !== 1) {
    fail("Vite config must contain exactly one default export");
  }
  const defineCall = unwrapParentheses(exports[0].expression);
  if (
    !ts.isCallExpression(defineCall) ||
    !ts.isIdentifier(defineCall.expression) ||
    defineCall.expression.text !== defineConfigName ||
    defineCall.arguments.length !== 1
  ) {
    fail("Vite config must export one direct defineConfig factory");
  }
  const factory = unwrapParentheses(defineCall.arguments[0]);
  if (!ts.isArrowFunction(factory) || ts.isBlock(factory.body)) {
    fail("Vite config must use an expression-bodied defineConfig factory");
  }
  const config = unwrapParentheses(factory.body);
  if (!ts.isObjectLiteralExpression(config)) {
    fail("Vite config factory must return one object literal");
  }
  if (
    config.properties.some(
      (property) =>
        ts.isSpreadAssignment(property) ||
        (property.name && ts.isComputedPropertyName(property.name)),
    )
  ) {
    fail("Vite config cannot use spread or computed top-level properties");
  }
  function rejectPrototypeProperties(node) {
    if (
      ts.isPropertyAssignment(node) &&
      node.name &&
      propertyName(node.name) === "__proto__"
    ) {
      fail("Vite config cannot use __proto__ properties");
    }
    ts.forEachChild(node, rejectPrototypeProperties);
  }
  rejectPrototypeProperties(config);
  const pluginProperties = config.properties.filter(
    (property) => property.name && propertyName(property.name) === "plugins",
  );
  if (
    pluginProperties.length !== 1 ||
    !ts.isPropertyAssignment(pluginProperties[0]) ||
    !ts.isArrayLiteralExpression(pluginProperties[0].initializer)
  ) {
    fail("Vite config must contain one literal plugins array");
  }
  const plugins = pluginProperties[0].initializer.elements;
  const expectedFactories = [reactName, tailwindName];
  if (plugins.length !== 3) {
    fail("Vite config must contain exactly the approved React, Tailwind, and serve-prefix plugins");
  }
  for (let index = 0; index < expectedFactories.length; index += 1) {
    const plugin = unwrapParentheses(plugins[index]);
    if (
      !ts.isCallExpression(plugin) ||
      !ts.isIdentifier(plugin.expression) ||
      plugin.expression.text !== expectedFactories[index] ||
      plugin.arguments.length !== 0
    ) {
      fail("Vite config must instantiate the approved React and Tailwind plugin factories directly");
    }
  }
  const localPlugin = unwrapParentheses(plugins[2]);
  if (!ts.isObjectLiteralExpression(localPlugin)) {
    fail("Vite config serve-prefix plugin must be an object literal");
  }
  const localProperties = new Map();
  for (const property of localPlugin.properties) {
    const name = property.name ? propertyName(property.name) : null;
    if (!name || mapHas(localProperties, name)) {
      fail("Vite config serve-prefix plugin has an unsupported property");
    }
    localProperties.set(name, property);
  }
  if (
    localProperties.size !== 3 ||
    !mapHas(localProperties, "name") ||
    !mapHas(localProperties, "apply") ||
    !mapHas(localProperties, "configureServer")
  ) {
    fail("Vite config serve-prefix plugin must contain only name, apply, and configureServer");
  }
  const nameProperty = localProperties.get("name");
  const applyProperty = localProperties.get("apply");
  if (
    !ts.isPropertyAssignment(nameProperty) ||
    literalText(nameProperty.initializer) !== serveOnlyVitePluginName ||
    !ts.isPropertyAssignment(applyProperty) ||
    literalText(applyProperty.initializer) !== "serve" ||
    !ts.isMethodDeclaration(localProperties.get("configureServer"))
  ) {
    fail("Vite config serve-prefix plugin must retain its fixed name and serve-only hook");
  }
}

function verifyNoNestedVitePluginOptions(config) {
  const seen = new WeakSet();

  function visit(value, location, root) {
    if (typeof value === "function") {
      fail(`Vite configuration contains a function-valued container at ${location || "root"}`);
    }
    if (typeof value !== "object" || value === null) {
      return;
    }
    if (weakSetHas(seen, value)) {
      return;
    }
    seen.add(value);
    const prototype = objectGetPrototypeOf(value);
    if (
      (!arrayIsArray(value) && prototype !== Object.prototype && prototype !== null) ||
      (arrayIsArray(value) && prototype !== Array.prototype)
    ) {
      fail(`Vite configuration contains a non-plain object at ${location || "root"}`);
    }
    for (const key of enumerableOwnKeys(value, location)) {
      if (root && key === "plugins") {
        continue;
      }
      const child = value[key];
      const childLocation = location ? `${location}.${key}` : key;
      if (key === "plugins" && child != null && child !== false) {
        fail(`Vite configuration contains nested plugin options at ${childLocation}`);
      }
      visit(child, childLocation, false);
    }
  }

  visit(config, "", true);
}

function verifyServePrefixBehavior(plugin) {
  const middleware = [];
  plugin.configureServer({
    middlewares: {
      use(handler) {
        middleware.push(handler);
      },
    },
  });
  if (middleware.length !== 1 || typeof middleware[0] !== "function") {
    fail("Vite serve-prefix plugin must install exactly one middleware");
  }
  for (const [initial, expected] of [
    ["/_app/probe.js?mode=guard", "/probe.js?mode=guard"],
    ["/api/probe", "/api/probe"],
  ]) {
    const request = { url: initial };
    let nextCalls = 0;
    middleware[0](request, {}, () => {
      nextCalls += 1;
    });
    if (request.url !== expected || nextCalls !== 1) {
      fail("Vite serve-prefix middleware no longer mirrors the gateway strip-prefix behavior");
    }
  }
}

async function flattenUserPluginOptions(options, flattened = []) {
  const resolved = await options;
  if (resolved == null || resolved === false) {
    return flattened;
  }
  if (arrayIsArray(resolved)) {
    for (const option of resolved) {
      await flattenUserPluginOptions(option, flattened);
    }
    return flattened;
  }
  flattened.push(resolved);
  return flattened;
}

async function loadViteUserConfig(webRoot, configFile, command) {
  const loaded = await loadConfigFromFile(
    { command, mode: command === "build" ? "production" : "development" },
    configFile,
    webRoot,
    "silent",
  );
  if (!loaded?.config) {
    fail("Vite build configuration could not be loaded");
  }
  return loaded.config;
}

async function verifyVitePlugins(config) {
  const plugins = await flattenUserPluginOptions(config.plugins);
  const names = plugins.map((plugin) => plugin?.name);
  if (
    names.length !== expectedVitePluginNames.length ||
    names.some((name, index) => name !== expectedVitePluginNames[index])
  ) {
    fail("Vite configuration must resolve exactly the approved user plugin sequence");
  }
  for (const plugin of plugins) {
    if (!plugin || typeof plugin !== "object" || typeof plugin.name !== "string") {
      fail("Vite configuration contains an unnamed plugin option");
    }
    if (plugin.name === serveOnlyVitePluginName) {
      if (plugin.apply !== "serve") {
        fail(`${serveOnlyVitePluginName} must be serve-only`);
      }
      verifyServePrefixBehavior(plugin);
    }
  }
}

export async function runGuard(repoRoot = process.env.ZEROCLAW_RSC_GUARD_ROOT ?? defaultRepoRoot) {
  const resolvedRepoRoot = pathResolve(repoRoot);
  const webPath = pathJoin(resolvedRepoRoot, "web");
  if (!fsExistsSync(webPath) || !fsLstatSync(webPath).isDirectory()) {
    fail("missing web directory");
  }
  if (fsLstatSync(webPath).isSymbolicLink()) {
    fail("web directory is a symbolic link");
  }
  const webRoot = fsRealpathSync(webPath);
  const webSourceRoot = fsRealpathSync(pathJoin(webRoot, "src"));
  const nodeModulesRoot = pathJoin(webRoot, "node_modules");
  const distRoot = pathJoin(webRoot, "dist");
  const configCandidates = ["js", "mjs", "cjs", "ts", "mts", "cts"]
    .map((extension) => pathJoin(webRoot, `vite.config.${extension}`))
    .filter((candidate) => fsExistsSync(candidate));
  if (configCandidates.length !== 1) {
    fail("guarded web root must contain exactly one Vite config file");
  }
  const configFile = fsRealpathSync(configCandidates[0]);
  assertInsideWebRoot(configFile, webRoot, nodeModulesRoot, "Vite config", true);
  verifyViteConfigHasNoPrototypeMutation(configFile);
  const packagePath = pathJoin(webRoot, "package.json");
  const declared = declaredPackageNames(packagePath);
  const guardedSourceFiles = collectSourceFiles(webRoot, webSourceRoot);
  const guardedInputSnapshot = captureGuardedInputSnapshot(packagePath, configFile);
  const records = [];
  for (const filePath of guardedSourceFiles) {
    if (isServerEntry(filePath)) {
      fail(`${relativePath(webRoot, filePath)} is a server/RSC entry surface`);
    }
    const source = fsReadFileSync(filePath, "utf8");
    guardedInputSnapshot.set(filePath, source);
    if (pathExtname(filePath).toLowerCase() === ".html") {
      inspectHtml(filePath, source, records, webRoot, nodeModulesRoot, distRoot);
    } else {
      inspectSource(filePath, source, records);
    }
  }
  let server;
  try {
    server = await loadViteServer(webRoot, configFile);
    verifyGuardedInputSnapshot(
      guardedInputSnapshot,
      packagePath,
      configFile,
      webRoot,
      webSourceRoot,
      "Vite server config execution",
    );
    if (pathResolve(server.config.root) !== webRoot) {
      fail("effective Vite root escapes the guarded web root");
    }
    if (typeof server.config.configFile !== "string") {
      fail("effective Vite configuration must use one config file");
    }
    if (fsRealpathSync(server.config.configFile) !== configFile) {
      fail("effective Vite configuration does not match the prechecked config file");
    }
    for (const record of records) {
      await verifyImportBoundary(
        server,
        record,
        webRoot,
        webSourceRoot,
        nodeModulesRoot,
        distRoot,
        declared,
      );
    }

    verifyEffectiveAlias(server, webRoot, webSourceRoot, nodeModulesRoot);
    verifyViteConfigPluginSource(configFile);
    const serveConfig = await loadViteUserConfig(webRoot, configFile, "serve");
    verifyGuardedInputSnapshot(
      guardedInputSnapshot,
      packagePath,
      configFile,
      webRoot,
      webSourceRoot,
      "Vite serve config execution",
    );
    const buildConfig = await loadViteUserConfig(webRoot, configFile, "build");
    verifyGuardedInputSnapshot(
      guardedInputSnapshot,
      packagePath,
      configFile,
      webRoot,
      webSourceRoot,
      "Vite build config execution",
    );
    verifyViteConfigShape(serveConfig, webRoot, webSourceRoot, nodeModulesRoot);
    verifyViteConfigShape(buildConfig, webRoot, webSourceRoot, nodeModulesRoot);
    await verifyVitePlugins(serveConfig);
    await verifyVitePlugins(buildConfig);
  } finally {
    if (server) {
      await server.close();
    }
  }
  verifyGuardedInputSnapshot(
    guardedInputSnapshot,
    packagePath,
    configFile,
    webRoot,
    webSourceRoot,
    "successful guard completion",
  );
}

const isMain = process.argv[1] && pathResolve(process.argv[1]) === scriptPath;
if (isMain) {
  try {
    await runGuard();
    console.log(`${errorPrefix} client-only React Router boundary verified`);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    console.error(message.startsWith(errorPrefix) ? message : `${errorPrefix} ${message}`);
    process.exitCode = 1;
  }
}
