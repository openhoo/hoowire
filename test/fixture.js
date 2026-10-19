function Composer({ref:ref,onSendMessage:send}) {
  let {selectedChat:chat}=chatState(), {getUserSetting:setting}=userSettings();
  let allowed=policy("allowAttachments"), photos=policy("allowAccessPhotos"), recording=state(), files=[], type="normal";
  let attach=React.useCallback(async incoming=>{for(const file of incoming) await asset.getAssetFromFile({file})},[]);
  React.useImperativeHandle(ref,()=>({height:0,onSelectedFiles:attach}));
  return React.useEffect(()=>{},[chat]), jsx("div",{children:[
    jsx(Emoji,{onCloseEmojiPicker:closeEmoji,onSelectEmoji:chooseEmoji,ref:ref}),
    jsx(Input,{"data-testid":"workspace-text-input",disabled:recording}),
    jsx(Attachments,{isAttachmentsButtonDisabled:type!=="normal"&&files.length>0,onSelectedFiles:attach})
  ]});
}
