'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { transform, validateMain } = require('../scripts/transform.cjs');
const fixture = fs.readFileSync(path.join(__dirname, 'fixture.js'), 'utf8');

function render(source, allowed = true, recording = false) {
  const state = { effects: [], callback: null, opened: null };
  const context = {
    chatState: () => ({ selectedChat: 'chat-17' }), userSettings: () => ({ getUserSetting: () => 'de' }),
    policy: () => allowed, state: () => recording, jsx: (type, props) => ({ type, props }),
    Emoji: 'emoji', Input: 'input', Attachments: 'attachments', closeEmoji() {}, chooseEmoji() {},
    asset: { getAssetFromFile() {} },
    React: { useCallback: callback => callback, useEffect: callback => state.effects.push(callback),
      useImperativeHandle: (ref, callback) => { state.callback = callback().onSelectedFiles; } },
    window: { teamwireGifPicker: { label: () => 'GIFs suchen', open: options => { state.opened = options; }, closeForContext: chat => { state.closed = chat; } } },
  };
  vm.createContext(context);
  // Only this authored fixture is executed. Real vendor code is parsed only.
  const result = vm.runInContext(source + '\nComposer({ref:{},onSendMessage(){}})', context);
  return { result, state };
}

test('adds a button bound to the actual chat and existing attachment callback', () => {
  const { result, state } = render(transform(fixture).output);
  const button = result.props.children[1];
  assert.equal(button.type, 'button');
  assert.equal(button.props['aria-label'], 'GIFs suchen');
  button.props.onClick({ currentTarget: 'anchor' });
  assert.equal(state.opened.contextKey, 'chat-17');
  assert.equal(state.opened.onSelectedFiles, state.callback);
  state.effects[0]()();
  assert.equal(state.closed, 'chat-17');
});

test('survives minified identifier renaming, including an event-named callback', () => {
  let renamed = fixture.replace(/\battach\b/g, 'event').replace(/\bchat\b/g, 'n7').replace(/\bsetting\b/g, 'q9')
    .replace(/\ballowed\b/g, 'a0').replace(/\bphotos\b/g, 'a1');
  const transformed = transform(renamed);
  const { result, state } = render(transformed.output);
  result.props.children[1].props.onClick({ currentTarget: 'different-anchor' });
  assert.equal(state.opened.onSelectedFiles, state.callback);
  assert.equal(state.opened.contextKey, 'chat-17');
});

test('obeys existing attachment policies and recording-disabled state', () => {
  const output = transform(fixture).output;
  assert.equal(render(output, false).result.props.children[1], null);
  assert.equal(render(output, true, true).result.props.children[1].props.disabled, true);
});

test('rejects duplicate patching, ambiguous composers and broken attachment anchors', () => {
  assert.throws(() => transform(transform(fixture).output), /already contains/);
  assert.throws(() => transform(fixture + fixture.replace('Composer', 'OtherComposer')), /expected one match/);
  assert.throws(() => transform(fixture.replace('getAssetFromFile', 'differentFileAPI')), /expected one match/);
  assert.throws(() => transform(fixture.replace('onSelectEmoji', 'onSelectSticker')), /emoji picker/);
});

test('rejects changed Electron security or preload models', () => {
  const main = 'new electron.BrowserWindow({webPreferences:{contextIsolation:true,nodeIntegration:false,sandbox:false,preload:path.join(__dirname,"preloads/main.js")}})';
  assert.equal(validateMain(main), true);
  assert.throws(() => validateMain(main.replace('nodeIntegration:false', 'nodeIntegration:true')), /security/);
  assert.throws(() => validateMain(main.replace('preloads/main.js', 'preloads/changed.js')), /expected one match/);
});
