//! Мини-калькулятор для полей инспектора: «как в Figma» можно вводить
//! арифметические выражения (`100+50`, `2*8`, `(400-120)/2`, `50%`, ...).
//!
//! Поддерживаются `+ - * / ( )`, десятичные числа, пробелы и суффикс `%`
//! (деление на 100). Обработка ошибок мягкая: нераспознанное выражение
//! возвращает `None`, и вызывающая сторона игнорирует ввод.

/// Пытается распарсить строку как число или арифметическое выражение.
pub fn eval(input: &str) -> Option<f32> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    let mut parser = Parser { tokens: tokenize(s)?, pos: 0 };
    let value = parser.expr()?;
    if parser.pos != parser.tokens.len() {
        return None;
    }
    Some(value)
}

/// Результат разбора поля инспектора: готовое число или процент от базы поля.
/// `50%` для W/H — процент от размера родительского фрейма, для радиуса —
/// от меньшей стороны фигуры. Арифметика (`100*50%`) даёт готовое число
/// (50% внутри выражения = 0.5).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    /// Число как есть (результат арифметики).
    Plain(f32),
    /// Процент от базы поля: 50% → 0.5.
    Percent(f32),
}

/// Разбирает ввод с учётом контекста: одиночный суффикс `%` — процент от базы,
/// иначе — обычное арифметическое выражение (внутри него `%` = деление на 100).
pub fn parse(input: &str) -> Option<Value> {
    let s = input.trim();
    if let Some(num) = s.strip_suffix('%') {
        let num = num.trim();
        if let Ok(n) = num.parse::<f32>() {
            return Some(Value::Percent(n / 100.0));
        }
        // «100*50%» — % внутри выражения обрабатывает eval.
        return eval(s).map(Value::Plain);
    }
    eval(s).map(Value::Plain)
}

/// Число или простое выражение без пробелов между токенами? Нет — выражение
/// может содержать пробелы; `parse::<f32>` покрывает чистые числа, но для
/// единообразия весь парсинг идёт через `eval`.

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f32),
    Plus,
    Minus,
    Star,
    Slash,
    LParen,
    RParen,
}

fn tokenize(s: &str) -> Option<Vec<Tok>> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '+' => out.push(Tok::Plus),
            '-' => out.push(Tok::Minus),
            '*' => out.push(Tok::Star),
            '/' => out.push(Tok::Slash),
            '(' => out.push(Tok::LParen),
            ')' => out.push(Tok::RParen),
            '%' => {
                // Суффикс «процентов» к предыдущему числу: 50% = 0.5.
                if let Some(Tok::Num(n)) = out.last_mut() {
                    *n /= 100.0;
                } else {
                    return None;
                }
            }
            '0'..='9' | '.' => {
                // Собираем число (допускаем «.5», «5.», «1.25»).
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                out.push(Tok::Num(text.parse().ok()?));
                continue;
            }
            _ => return None,
        }
        i += 1;
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

struct Parser {
    tokens: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    /// expr := term (('+' | '-') term)*
    fn expr(&mut self) -> Option<f32> {
        let mut value = self.term()?;
        while let Some(tok) = self.peek().cloned() {
            match tok {
                Tok::Plus => {
                    self.next();
                    value += self.term()?;
                }
                Tok::Minus => {
                    self.next();
                    value -= self.term()?;
                }
                _ => break,
            }
        }
        Some(value)
    }

    /// term := factor (('*' | '/') factor)*
    fn term(&mut self) -> Option<f32> {
        let mut value = self.factor()?;
        while let Some(tok) = self.peek().cloned() {
            match tok {
                Tok::Star => {
                    self.next();
                    value *= self.factor()?;
                }
                Tok::Slash => {
                    self.next();
                    let d = self.factor()?;
                    if d.abs() < f32::EPSILON {
                        return None;
                    }
                    value /= d;
                }
                _ => break,
            }
        }
        Some(value)
    }

    /// factor := ('-' | '+') factor | number | '(' expr ')'
    fn factor(&mut self) -> Option<f32> {
        let t = self.peek()?.clone();
        match t {
            Tok::Minus => {
                self.next();
                Some(-self.factor()?)
            }
            Tok::Plus => {
                self.next();
                self.factor()
            }
            Tok::Num(n) => {
                self.next();
                Some(n)
            }
            Tok::LParen => {
                self.next();
                let v = self.expr()?;
                if self.next()? != Tok::RParen {
                    return None;
                }
                Some(v)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_numbers() {
        assert_eq!(eval("42"), Some(42.0));
        assert_eq!(eval(" -3.5 "), Some(-3.5));
        assert_eq!(eval(".5"), Some(0.5));
    }

    #[test]
    fn arithmetic() {
        assert_eq!(eval("100+50"), Some(150.0));
        assert_eq!(eval("100 - 50"), Some(50.0));
        assert_eq!(eval("2*8"), Some(16.0));
        assert_eq!(eval("100/4"), Some(25.0));
        assert_eq!(eval("(400-120)/2"), Some(140.0));
        assert_eq!(eval("2*(3+4)"), Some(14.0));
        assert_eq!(eval("10-2*3"), Some(4.0));
        assert_eq!(eval("50%"), Some(0.5));
        assert_eq!(eval("100*50%"), Some(50.0));
    }

    #[test]
    fn invalid_inputs() {
        assert_eq!(eval(""), None);
        assert_eq!(eval("abc"), None);
        assert_eq!(eval("100/0"), None);
        assert_eq!(eval("(1+2"), None);
        assert_eq!(eval("1 2"), None);
        assert_eq!(eval("--5"), Some(5.0));
    }

    #[test]
    fn parse_percent_vs_plain() {
        assert_eq!(parse("50%"), Some(Value::Percent(0.5)));
        assert_eq!(parse("25.5%"), Some(Value::Percent(0.255)));
        assert_eq!(parse("100"), Some(Value::Plain(100.0)));
        assert_eq!(parse("100+50"), Some(Value::Plain(150.0)));
        assert_eq!(parse("100*50%"), Some(Value::Plain(50.0)));
        assert_eq!(parse(""), None);
        assert_eq!(parse("abc"), None);
        assert_eq!(parse("%"), None);
    }
}