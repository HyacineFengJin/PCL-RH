// A deliberately small authoring helper, shared by the offline HTML form and
// the Node example workflow. Admission still uses readManifest, not this form.
export function createAuthorManifest(input) {
  const fail=message=>{throw new Error(message);};
  const text=(name,value,max,multiline=false)=>{
    if(typeof value!=='string')fail(`${name}必须是文字`);
    const result=value.trim();
    if(!result||[...result].length>max||/[\u0000-\u0008\u000b-\u001f\u007f-\u009f\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069\ud800-\udfff]/u.test(result)||(!multiline&&/[\n\t]/.test(result)))
      fail(`${name}为空、过长或包含不支持的控制字符`);
    return result;
  };
  const id=text('扩展 ID',input.id,128);
  if(!/^[a-z][a-z0-9-]*(?:\.[a-z][a-z0-9-]*){1,7}(?![\s\S])/.test(id))fail('扩展 ID 请使用小写英文、数字、短横线和至少一个点，如 local.my-tool');
  const version=text('版本',input.version,20);
  if(!/^(0|[1-9]\d{0,5})\.(0|[1-9]\d{0,5})\.(0|[1-9]\d{0,5})(?![\s\S])/.test(version))fail('版本请使用 1.0.0 这样的三段稳定版本号');
  if(!['home.secondary','tools.cards','rh-code.cards','settings.cards'].includes(input.slot))fail('请选择已知卡片位置');
  const scope=Object.hasOwn(input,'scope')?input.scope:(input.slot==='rh-code.cards'?'rh-code':'launcher');
  if(!['launcher','rh-code','both'].includes(scope)||(scope!=='both'&&scope!==(input.slot==='rh-code.cards'?'rh-code':'launcher')))
    fail('运行范围与卡片位置不匹配');
  if(typeof input.navigationEnabled!=='boolean'||typeof input.summaryEnabled!=='boolean')fail('请选择明确的功能开关');
  const capabilities=[{id:'ui.cards',required:true,reason:'在宿主提供的位置显示本扩展的纯文字卡片。'}],actions=[];
  if(input.navigationEnabled){
    if(!['launch','instances','downloads','tools','settings'].includes(input.navigationTarget))fail('请选择固定启动器页面');
    capabilities.push({id:'launcher.navigate',required:false,reason:'用户点击后切换到启动器已有页面，不启动游戏或执行文件操作。'});
    actions.push({id:'navigate',label:text('导航按钮',input.navigationLabel,64),kind:'navigate',target:input.navigationTarget});
  }
  if(input.summaryEnabled){
    capabilities.push({id:'instances.summary',required:false,reason:'用户点击后查看当前实例的游戏版本、加载器、模组数量与隔离状态，不读取路径或账号。'});
    actions.push({id:'summary',label:'查看实例摘要',kind:'show-instance-summary'});
  }
  return {
    format:'pcl-linux.declarative-extension',schemaVersion:1,
    id,name:text('扩展名称',input.name,80),version,publisher:text('作者名称',input.publisher,80),
    ...(Object.hasOwn(input,'scope')||input.slot==='rh-code.cards'?{scope}:{}),
    api:{min:1,maxExclusive:2},capabilities,
    contributions:{cards:[{id:'main-card',slot:input.slot,title:text('卡片标题',input.title,100),text:text('卡片正文',input.text,2000,true),actions}]},
  };
}
