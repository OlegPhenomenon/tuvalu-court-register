import type { ButtonHTMLAttributes } from 'react';

/** `<Button variant="primary|secondary|danger" busy={saving} onClick={…}>`. `busy` disables and marks aria-busy. */
export function Button({ variant = 'primary', busy, className, children, disabled, ...rest }: {
  variant?: 'primary' | 'secondary' | 'danger';
  busy?: boolean;
} & ButtonHTMLAttributes<HTMLButtonElement>) {
  return (
    <button
      className={`btn btn--${variant}${busy ? ' is-busy' : ''}${className ? ` ${className}` : ''}`}
      disabled={disabled || busy}
      aria-busy={busy || undefined}
      {...rest}
    >
      {children}
    </button>
  );
}
