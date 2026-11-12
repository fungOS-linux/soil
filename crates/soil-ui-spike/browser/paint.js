// Paint the view's commands onto a 2D context. Pure enough to run against a
// stub, which is how the page's painter is tested without a browser.

const FONT = 'system-ui, -apple-system, "Helvetica Neue", Arial, sans-serif';

export function paint(commands, ctx, width, height) {
  ctx.save();
  ctx.clearRect(0, 0, width, height);
  let layers = 0;

  for (const command of commands) {
    switch (command[0]) {
      case "rect": {
        const [, x, y, w, h, radius, colour] = command;
        ctx.fillStyle = colour ?? "#000";
        if (radius > 0 && typeof ctx.roundRect === "function") {
          ctx.beginPath();
          ctx.roundRect(x, y, w, h, radius);
          ctx.fill();
        } else {
          ctx.fillRect(x, y, w, h);
        }
        break;
      }
      case "text": {
        const [, value, x, y, size, colour, anchor, align, , wrap] = command;
        ctx.fillStyle = colour ?? "#fff";
        ctx.font = `${size}px ${FONT}`;
        ctx.textAlign = align === "center" ? "center" : align === "right" ? "right" : "left";
        ctx.textBaseline =
          anchor === "baseline" ? "alphabetic" : anchor === "top" ? "top" : "middle";
        const lines = wrap ? wrapText(ctx, value, wrap) : [value];
        const step = size * 1.25;
        const first = anchor === "middle" ? y - ((lines.length - 1) * step) / 2 : y;
        lines.forEach((line, i) => ctx.fillText(line, x, first + i * step));
        break;
      }
      case "clip":
        ctx.save();
        ctx.beginPath();
        ctx.rect(command[1], command[2], command[3], command[4]);
        ctx.clip();
        layers++;
        break;
      case "clip-end":
        if (layers > 0) {
          ctx.restore();
          layers--;
        }
        break;
      case "hit":
        // A hit region is for input, not pixels. The host keeps those.
        break;
    }
  }

  while (layers-- > 0) ctx.restore();
  ctx.restore();
  return commands.length;
}

function wrapText(ctx, value, width) {
  const lines = [];
  let line = "";
  for (const word of String(value).split(/\s+/)) {
    const next = line ? `${line} ${word}` : word;
    if (line && ctx.measureText(next).width > width) {
      lines.push(line);
      line = word;
    } else {
      line = next;
    }
  }
  if (line) lines.push(line);
  return lines;
}
