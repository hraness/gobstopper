# Writes the synthetic Claude Code session the mockup fixtures were captured
# from: a made-up user ("sam") and project ("lanternshop"). Run from an empty
# directory; see ../README.md for the capture steps. Deterministic (seed 11).
import os
import json, uuid, random
random.seed(11)
sid="5f0c2a9e-7b1d-4c3e-9a55-2d8e61f0b4a7"
out=[]; parent=None
def add(kind, content, usage=None):
    global parent
    u=str(uuid.UUID(int=random.getrandbits(128)))
    msg={"role":kind,"content":content}
    if usage: msg["usage"]=usage
    rec={"type":kind,"uuid":u,"parentUuid":parent,"sessionId":sid,"cwd":"/Users/sam/code/lanternshop","timestamp":"2026-10-01T%02d:%02d:00Z"%(9+len(out)//60,len(out)%60),"message":msg}
    out.append(rec); parent=u
add("user","Add a coupon field to the checkout page and make the tests pass.")
files=["src/checkout/cart.ts","src/checkout/totals.ts","src/checkout/coupon.ts","src/api/orders.ts","src/ui/CheckoutForm.tsx"]
for i in range(140):
    tid=f"toolu_{i:04d}"
    if i%3==0:
        name="Bash"; inp={"command":"bun test tests/checkout.test.ts"}
        fail = i < 120
        lines=[f"tests/checkout.test.ts:"]
        for t in range(42):
            ok = not (fail and t in (7,19,31))
            lines.append(("(pass) " if ok else "(fail) ")+f"checkout > case {t+1}")
        if fail:
            lines.append("error: expect(received).toBe(expected)\n  Expected: 90.00\n  Received: 100.00\n  at applyCoupon (src/checkout/coupon.ts:14:10) code SAVE10")
        lines.append(f" {39 if fail else 42} pass\n {3 if fail else 0} fail\nRan 42 tests across 1 file.")
        body="\n".join(lines*random.randint(2,4))
    else:
        f=files[i%len(files)]; name="Read"; inp={"file_path":f}
        body="".join(f"{n+1}\t  const line{n} = compute{n}(cart, coupon); // {f}\n" for n in range(random.randint(60,140)))
    add("assistant",[{"type":"text","text":f"Checking the coupon logic."},{"type":"tool_use","id":tid,"name":name,"input":inp}],{"input_tokens":1000+i*900,"output_tokens":120})
    add("user",[{"type":"tool_result","tool_use_id":tid,"content":body}])
add("assistant",[{"type":"text","text":"The coupon field is in place and all 42 checkout tests pass."}],{"input_tokens":130000,"output_tokens":60})
os.makedirs("home/.claude/projects/-Users-sam-code-lanternshop", exist_ok=True)
open("home/.claude/projects/-Users-sam-code-lanternshop/%s.jsonl"%sid,"w").write("\n".join(json.dumps(r) for r in out)+"\n")
