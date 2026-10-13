import { Dialog as RDialog, Switch as RSwitch, Tooltip as RTooltip } from "radix-ui";
import { forwardRef, type ButtonHTMLAttributes, type InputHTMLAttributes, type ReactNode, type SelectHTMLAttributes, type TextareaHTMLAttributes } from "react";
import { cn } from "../lib/utils";

type Variant = "primary" | "run" | "secondary" | "ghost" | "danger";
type Size = "sm" | "md";

const variants: Record<Variant, string> = {
  primary: "bg-accent text-accent-fg hover:brightness-105 active:brightness-95 shadow-sm",
  run: "bg-ok text-white hover:brightness-105 active:brightness-95 shadow-sm",
  secondary: "bg-raised text-fg border border-line-strong hover:bg-hover shadow-[0_1px_0_rgb(0_0_0/0.03)]",
  ghost: "text-muted hover:text-fg hover:bg-hover",
  danger: "text-err hover:bg-err-soft",
};

export const Button = forwardRef<
  HTMLButtonElement,
  ButtonHTMLAttributes<HTMLButtonElement> & { variant?: Variant; size?: Size }
>(function Button({ className, variant = "secondary", size = "md", ...props }, ref) {
  return (
    <button
      ref={ref}
      className={cn(
        "inline-flex items-center justify-center gap-1.5 rounded-md font-medium whitespace-nowrap transition-[background-color,filter,color] duration-100 disabled:opacity-45 disabled:pointer-events-none",
        size === "sm" ? "h-6 px-2 text-xs" : "h-7 px-3 text-[13px]",
        variants[variant],
        className,
      )}
      {...props}
    />
  );
});

export function IconButton({
  label,
  className,
  children,
  shortcut,
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { label: string; shortcut?: string }) {
  return (
    <Tip label={label} shortcut={shortcut}>
      <button
        aria-label={label}
        className={cn(
          "inline-flex size-7 items-center justify-center rounded-md text-muted transition-colors duration-100 hover:bg-hover hover:text-fg disabled:opacity-40 disabled:pointer-events-none",
          className,
        )}
        {...props}
      >
        {children}
      </button>
    </Tip>
  );
}

export function Tip({ label, shortcut, children }: { label: string; shortcut?: string; children: ReactNode }) {
  return (
    <RTooltip.Root delayDuration={500}>
      <RTooltip.Trigger asChild>{children}</RTooltip.Trigger>
      <RTooltip.Portal>
        <RTooltip.Content
          side="bottom"
          sideOffset={6}
          className="z-50 flex items-center gap-2 rounded-md bg-fg px-2 py-1 text-xs text-bg shadow-lg"
        >
          {label}
          {shortcut && <span className="opacity-60">{shortcut}</span>}
        </RTooltip.Content>
      </RTooltip.Portal>
    </RTooltip.Root>
  );
}

const fieldBase =
  "w-full rounded-md border border-line-strong bg-raised px-2.5 text-[13px] text-fg placeholder:text-faint shadow-[inset_0_1px_1px_rgb(0_0_0/0.03)] outline-none transition-[border-color,box-shadow] duration-100 focus:border-accent focus:ring-3 focus:ring-accent-soft disabled:opacity-60";

export const Input = forwardRef<HTMLInputElement, InputHTMLAttributes<HTMLInputElement> & { mono?: boolean }>(
  function Input({ className, mono, ...props }, ref) {
    return (
      <input
        ref={ref}
        spellCheck={false}
        autoCorrect="off"
        autoCapitalize="off"
        className={cn(fieldBase, "h-7", mono && "font-mono text-[12px]", className)}
        {...props}
      />
    );
  },
);

export const Textarea = forwardRef<HTMLTextAreaElement, TextareaHTMLAttributes<HTMLTextAreaElement> & { mono?: boolean }>(
  function Textarea({ className, mono, ...props }, ref) {
    return (
      <textarea
        ref={ref}
        spellCheck={false}
        autoCorrect="off"
        autoCapitalize="off"
        className={cn(fieldBase, "py-1.5 leading-relaxed resize-none", mono && "font-mono text-[12px]", className)}
        {...props}
      />
    );
  },
);

export function Select({ className, children, ...props }: SelectHTMLAttributes<HTMLSelectElement>) {
  return (
    <select className={cn(fieldBase, "h-7 appearance-auto pr-1", className)} {...props}>
      {children}
    </select>
  );
}

export function Switch({
  checked,
  onChange,
  label,
  hint,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: ReactNode;
  hint?: ReactNode;
}) {
  return (
    <label className="flex cursor-default items-start gap-2.5 py-1">
      <RSwitch.Root
        checked={checked}
        onCheckedChange={onChange}
        className="relative mt-0.5 h-[18px] w-[30px] shrink-0 rounded-full bg-line-strong transition-colors duration-150 data-[state=checked]:bg-accent"
      >
        <RSwitch.Thumb className="block size-[14px] translate-x-[2px] rounded-full bg-white shadow transition-transform duration-150 data-[state=checked]:translate-x-[14px]" />
      </RSwitch.Root>
      <span className="flex flex-col">
        <span className="text-[13px] text-fg">{label}</span>
        {hint && <span className="text-xs text-muted">{hint}</span>}
      </span>
    </label>
  );
}

export function Field({
  label,
  hint,
  error,
  children,
  className,
  aside,
}: {
  label: ReactNode;
  hint?: ReactNode;
  error?: string | null;
  children: ReactNode;
  className?: string;
  aside?: ReactNode;
}) {
  return (
    <div className={cn("flex flex-col gap-1.5", className)}>
      <div className="flex items-baseline justify-between gap-2">
        <span className="text-xs font-medium text-muted">{label}</span>
        {aside}
      </div>
      {children}
      {error ? <span className="text-xs text-err">{error}</span> : hint ? <span className="text-xs text-faint">{hint}</span> : null}
    </div>
  );
}

export function Kbd({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <kbd
      className={cn(
        "inline-flex h-[18px] min-w-[18px] items-center justify-center rounded border border-line-strong bg-raised px-1 font-sans text-[10.5px] font-medium text-muted",
        className,
      )}
    >
      {children}
    </kbd>
  );
}

export function Badge({ children, className, tone = "neutral" }: { children: ReactNode; className?: string; tone?: "neutral" | "accent" | "ok" | "err" }) {
  const tones = {
    neutral: "bg-hover text-muted",
    accent: "bg-accent-soft text-accent",
    ok: "bg-ok-soft text-ok",
    err: "bg-err-soft text-err",
  };
  return (
    <span className={cn("inline-flex h-[18px] items-center gap-1 rounded px-1.5 text-[11px] font-medium whitespace-nowrap", tones[tone], className)}>
      {children}
    </span>
  );
}

export function Dialog({
  open,
  onOpenChange,
  title,
  description,
  children,
  className,
}: {
  open: boolean;
  onOpenChange: (o: boolean) => void;
  title: ReactNode;
  description?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <RDialog.Root open={open} onOpenChange={onOpenChange}>
      <RDialog.Portal>
        <RDialog.Overlay className="fixed inset-0 z-40 bg-black/25 data-[state=open]:animate-[fade-in_120ms_ease-out]" />
        <RDialog.Content
          // Don't close when the window regains focus from another app; Esc, Cancel and outside clicks still close.
          onFocusOutside={(e) => e.preventDefault()}
          className={cn(
            "fixed top-[18%] left-1/2 z-50 w-[min(520px,calc(100vw-32px))] -translate-x-1/2 rounded-xl border border-line-strong bg-raised p-5 shadow-2xl outline-none",
            className,
          )}
        >
          <RDialog.Title className="text-[15px] font-semibold text-fg">{title}</RDialog.Title>
          {description && <RDialog.Description className="mt-1 text-[13px] text-muted">{description}</RDialog.Description>}
          <div className="mt-4">{children}</div>
        </RDialog.Content>
      </RDialog.Portal>
    </RDialog.Root>
  );
}

export function TooltipProvider({ children }: { children: ReactNode }) {
  return <RTooltip.Provider>{children}</RTooltip.Provider>;
}
