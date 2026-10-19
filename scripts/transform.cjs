#!/usr/bin/env node
'use strict';

// Parse vendor code as data. Never load or execute the application bundle.
const fs = require('node:fs');
const acorn = require('../vendor/acorn/acorn.cjs');

function walk(root, visit) {
  const stack = [[root, null]];
  while (stack.length) {
    const [node, parent] = stack.pop();
    if (!node || typeof node.type !== 'string') continue;
    visit(node, parent);
    for (const value of Object.values(node)) {
      if (Array.isArray(value)) {
        for (let index = value.length - 1; index >= 0; index--) {
          if (value[index]?.type) stack.push([value[index], node]);
        }
      } else if (value?.type) stack.push([value, node]);
    }
  }
}

function string(node) {
  if (node?.type === 'Literal' && typeof node.value === 'string') return node.value;
  if (node?.type === 'TemplateLiteral' && !node.expressions.length) return node.quasis[0].value.cooked;
  return null;
}
function key(node) { return node?.type === 'Identifier' ? node.name : string(node); }
function property(object, name) {
  return object?.properties?.find(item => item.type === 'Property' && key(item.key) === name)?.value;
}
function one(values, description) {
  if (values.length !== 1) throw new Error(`${description}: expected one match, found ${values.length}`);
  return values[0];
}
function identifier(node, description) {
  if (node?.type !== 'Identifier') throw new Error(`${description}: expected an identifier`);
  return node.name;
}
function memberName(node) {
  if (node?.type === 'SequenceExpression') return memberName(node.expressions.at(-1));
  return node?.type === 'MemberExpression' ? key(node.property) : null;
}
function parse(source, sourceType = 'module') {
  return acorn.parse(source, { ecmaVersion: 'latest', sourceType });
}

function transform(source) {
  if (source.includes('teamwire-gif-button') || source.includes('window.teamwireGifPicker')) {
    throw new Error('Renderer already contains a GIF patch; use its original vendor backup.');
  }
  const ast = parse(source);
  const candidates = [];
  walk(ast, node => {
    if (['FunctionDeclaration', 'FunctionExpression', 'ArrowFunctionExpression'].includes(node.type) &&
        node.params.some(param => param.type === 'ObjectPattern' && property(param, 'onSendMessage'))) {
      candidates.push(node);
    }
  });
  const composer = one(candidates.filter(node => {
    const body = source.slice(node.start, node.end);
    return body.includes('workspace-text-input') && body.includes('allowAttachments') && body.includes('getAssetFromFile');
  }), 'chat composer');
  if (composer.body.type !== 'BlockStatement') throw new Error('Unexpected composer body.');

  const nodes = [], parents = new Map();
  walk(composer.body, (node, parent) => { nodes.push(node); parents.set(node, parent); });
  const objects = nodes.filter(node => node.type === 'ObjectExpression');
  const declarators = composer.body.body.filter(node => node.type === 'VariableDeclaration').flatMap(node => node.declarations);
  const extractBinding = name => identifier(one(declarators.filter(node => node.id.type === 'ObjectPattern' && property(node.id, name)), `${name} binding`).id.properties.find(item => key(item.key) === name).value, name);
  const policy = name => identifier(one(declarators.filter(node => node.init?.type === 'CallExpression' && string(node.init.arguments[0]) === name), `${name} policy`).id, name);
  const setting = extractBinding('getUserSetting');
  const chat = extractBinding('selectedChat');
  const attachmentsAllowed = policy('allowAttachments');
  const photosAllowed = policy('allowAccessPhotos');
  const input = one(objects.filter(node => string(property(node, 'data-testid')) === 'workspace-text-input'), 'composer text input');
  const disabled = property(input, 'disabled');
  if (!disabled) throw new Error('Missing composer disabled state.');
  const restriction = one(objects.filter(node => property(node, 'isAttachmentsButtonDisabled') && property(node, 'onSelectedFiles')), 'attachment restrictions');
  const callback = identifier(property(restriction, 'onSelectedFiles'), 'attachment callback');
  const callbackDeclaration = one(declarators.filter(node => node.id.type === 'Identifier' && node.id.name === callback), 'attachment callback definition');
  if (!source.slice(callbackDeclaration.start, callbackDeclaration.end).includes('getAssetFromFile')) {
    throw new Error('Attachment callback no longer imports browser files.');
  }
  const imperative = one(objects.filter(node => property(node, 'height') && property(node, 'onSelectedFiles')), 'exposed attachment callback');
  if (identifier(property(imperative, 'onSelectedFiles'), 'exposed callback') !== callback) {
    throw new Error('Attachment callback bindings disagree.');
  }
  const emojiProps = one(objects.filter(node => property(node, 'onCloseEmojiPicker') && property(node, 'onSelectEmoji')), 'emoji picker');
  const emoji = parents.get(emojiProps);
  if (emoji.type !== 'CallExpression' || emoji.arguments[1] !== emojiProps || parents.get(emoji)?.type !== 'ArrayExpression') {
    throw new Error('Emoji picker is no longer in the composer children array.');
  }
  const effectCalls = [...new Set(nodes.filter(node => node.type === 'CallExpression' && memberName(node.callee) === 'useEffect')
    .map(node => source.slice(node.callee.start, node.callee.end)))];
  const effect = one(effectCalls, 'React useEffect binding');
  const returns = composer.body.body.filter(node => node.type === 'ReturnStatement');
  const returnStatement = one(returns, 'composer return');
  const jsx = `(${source.slice(emoji.callee.start, emoji.callee.end)})`;
  const text = node => source.slice(node.start, node.end);
  let eventName = '__teamwireGifEvent';
  while (nodes.some(node => node.type === 'Identifier' && node.name === eventName)) eventName += '_';
  const blocked = `(${text(disabled)})||(${text(property(restriction, 'isAttachmentsButtonDisabled'))})`;
  const label = `window.teamwireGifPicker.label(${setting}("language"))`;
  const button = `${attachmentsAllowed}&&${photosAllowed}?${jsx}("button",{type:"button",title:${label},"aria-label":${label},"aria-haspopup":"dialog","aria-expanded":"false","data-testid":"teamwire-gif-button",disabled:${blocked},onMouseDown:${eventName}=>${eventName}.preventDefault(),onClick:${eventName}=>window.teamwireGifPicker.open({anchor:${eventName}.currentTarget,contextKey:${chat},language:${setting}("language"),onSelectedFiles:${callback}}),style:{flexShrink:0,height:28,minWidth:34,alignSelf:"center",display:"inline-flex",alignItems:"center",justifyContent:"center",marginRight:8,border:"1.5px solid currentColor",borderRadius:5,background:"transparent",color:"#6d788a",fontSize:10,fontWeight:750,letterSpacing:.2,padding:"0 4px",cursor:"pointer"},children:"GIF"}):null`;
  const edits = [
    { start: emoji.end, value: `,${button}` },
    { start: returnStatement.start, value: `(${effect})(()=>()=>window.teamwireGifPicker?.closeForContext(${chat}),[${chat}]);` },
  ].sort((a, b) => b.start - a.start);
  let output = source;
  for (const edit of edits) output = output.slice(0, edit.start) + edit.value + output.slice(edit.start);
  parse(output);
  return { output, probes: { composer: composer.id?.name || '(anonymous)', callback, chat, setting,
    attachmentsAllowed, photosAllowed, emojiOffset: emoji.start, cleanupOffset: returnStatement.start } };
}

// Validate the Electron window that actually uses the target preload.
function validateMain(source) {
  const ast = parse(source, 'script');
  const windows = [];
  walk(ast, node => {
    if (node.type !== 'NewExpression' || memberName(node.callee) !== 'BrowserWindow') return;
    const preferences = property(node.arguments[0], 'webPreferences');
    if (!preferences || !property(preferences, 'preload')) return;
    const preload = property(preferences, 'preload');
    const suffix = preload.type === 'CallExpression' ? string(preload.arguments.at(-1)) : null;
    if (suffix !== 'preloads/main.js') return;
    if (property(preferences, 'contextIsolation')?.value !== true || property(preferences, 'nodeIntegration')?.value !== false ||
        property(preferences, 'sandbox')?.value !== false) throw new Error('Main-window security or preload model changed.');
    windows.push(node);
  });
  one(windows, 'supported main BrowserWindow');
  return true;
}

if (require.main === module) {
  try {
    const request = JSON.parse(fs.readFileSync(0, 'utf8'));
    const result = request.operation === 'validate-main' ? { valid: validateMain(request.source) }
      : request.operation === 'validate' ? { valid: !!parse(request.source, request.sourceType || 'module') }
      : transform(request.source);
    process.stdout.write(JSON.stringify(result));
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
module.exports = { transform, validateMain, parse };
