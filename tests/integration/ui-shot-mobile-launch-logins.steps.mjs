// Mobile SPA in Chromium with fixture HTTP; no backend process is launched.
import { expect } from '@playwright/test';
export default async function ({page}) {
 const mesh={id:3,name:'Buildmesh',path:'/repo',created_at:'',scratchpad:'',sandbox:false,default_provider:'codex'};
 const capabilities={accepts_initial_prompt:true,requires_attention_hook:true};
 const providers=[{id:'codex',label:'Codex',harness_id:'codex',group_key:'codex',is_proxied:false,provider_id:null,icon:'C',color:'#00d4ff',capabilities,logins:[{id:'default',label:'Default',is_default:true,signed_in:true},{id:'work',label:'Work',is_default:false,signed_in:true},{id:'guest',label:'Guest',is_default:false,signed_in:false}]},{id:'launch/careful',label:'Careful review',harness_id:'codex',group_key:'codex',is_proxied:false,capabilities,configuration:{id:'launch/careful',name:'Careful review',spawn_option_id:'codex',model:null,effort:'high',extra_args:null}}];
 const requests=[];
 await page.route('**/api/**',async route=>{
  const path=new URL(route.request().url()).pathname;
  if(path==='/api/session'||path==='/api/ws-ticket') return route.fulfill({status:204});
  if(path==='/api/nodes/create') {
   requests.push(route.request().postDataJSON());
   return route.fulfill({json:{outcome:'start_failed',node:{id:17,mesh_id:3,name:'review',provider:'codex',status:'error'},start_error:'Fixture process refused'}});
  }
  const body=path==='/api/meshes'?[mesh]:path==='/api/providers'?providers:[];
  return route.fulfill({json:body});
 });
 await page.goto(`${new URL(page.url()).origin}/mobile/index.html`,{waitUntil:'domcontentloaded'});
 await page.getByRole('button',{name:'Work',exact:true}).click();
 const trigger=page.getByTestId('mesh-spawn-3-menu');
 await trigger.click();
 const sheet=page.getByRole('dialog');
 await expect(sheet).toBeVisible();
 await expect(sheet.getByRole('button',{name:/Codex · Guest/})).toBeDisabled();
 const choice=sheet.getByRole('button',{name:'Careful review · Work',exact:true});
 await choice.scrollIntoViewIfNeeded();
 expect((await choice.boundingBox()).height).toBeGreaterThanOrEqual(48);
 expect(await page.evaluate(()=>document.documentElement.scrollWidth <= innerWidth)).toBe(true);
 await page.screenshot({path:'.tmp/mobile-launch-after.png'});
 await page.keyboard.press('Escape');
 await expect(sheet).toHaveCount(0);
 await expect(trigger).toBeFocused();
 await trigger.click();
 await page.getByRole('button',{name:'Careful review · Work',exact:true}).click();
 await expect(page.getByLabel('Agent',{exact:true})).toHaveValue(JSON.stringify(['launch/careful','work']));
 await page.getByLabel('Your idea').fill('Review the mobile launch flows');
 await page.screenshot({path:'.tmp/mobile-launch-capture-after.png'});
 await page.getByRole('button',{name:'Start working on this',exact:true}).click();
 await expect(page.getByRole('button',{name:'Open the node',exact:true})).toBeVisible();
 expect(requests).toHaveLength(1);
 expect(requests[0]).toMatchObject({provider:'codex',configuration_id:'launch/careful',login_id:'work'});
 console.log('Phone web: grouped logins, unsigned row, touch bounds, Escape/focus return, Capture preset/payload, committed failure passed.');
}
