; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @main() {
entry:
  %t0 = add i32 0, 10
  %t1 = add i32 0, 5
  %t2 = add i32 %t0, %t1
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t2)
  %t4 = add i32 0, 3
  %t5 = sub i32 %t2, %t4
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t5)
  %t7 = add i32 0, 2
  %t8 = mul i32 %t5, %t7
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t8)
  %t10 = add i32 0, 4
  %t11 = sdiv i32 %t8, %t10
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t11)
  %t13 = add i32 0, 3
  %t14 = srem i32 %t11, %t13
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t14)
  ret i32 0
}

