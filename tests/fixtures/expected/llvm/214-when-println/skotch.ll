; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [8 x i8] c"January\00", align 1
@.str.1 = private unnamed_addr constant [9 x i8] c"February\00", align 1
@.str.2 = private unnamed_addr constant [6 x i8] c"March\00", align 1
@.str.3 = private unnamed_addr constant [6 x i8] c"April\00", align 1
@.str.4 = private unnamed_addr constant [4 x i8] c"May\00", align 1
@.str.5 = private unnamed_addr constant [5 x i8] c"June\00", align 1
@.str.6 = private unnamed_addr constant [6 x i8] c"Other\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)

define void @InputKt_printMonth(i32 %arg0) {
entry:
  %merge_2 = alloca void
  br label %bb1
bb1:
  %t0 = add i32 0, 1
  %t1 = icmp eq i32 %arg0, %t0
  br i1 %t1, label %bb2, label %bb3
bb2:
  call i32 @puts(ptr @.str.0)
  br label %bb14
bb3:
  %t3 = add i32 0, 2
  %t4 = icmp eq i32 %arg0, %t3
  br i1 %t4, label %bb4, label %bb5
bb4:
  call i32 @puts(ptr @.str.1)
  br label %bb14
bb5:
  %t6 = add i32 0, 3
  %t7 = icmp eq i32 %arg0, %t6
  br i1 %t7, label %bb6, label %bb7
bb6:
  call i32 @puts(ptr @.str.2)
  br label %bb14
bb7:
  %t9 = add i32 0, 4
  %t10 = icmp eq i32 %arg0, %t9
  br i1 %t10, label %bb8, label %bb9
bb8:
  call i32 @puts(ptr @.str.3)
  br label %bb14
bb9:
  %t12 = add i32 0, 5
  %t13 = icmp eq i32 %arg0, %t12
  br i1 %t13, label %bb10, label %bb11
bb10:
  call i32 @puts(ptr @.str.4)
  br label %bb14
bb11:
  %t15 = add i32 0, 6
  %t16 = icmp eq i32 %arg0, %t15
  br i1 %t16, label %bb12, label %bb13
bb12:
  call i32 @puts(ptr @.str.5)
  br label %bb14
bb13:
  call i32 @puts(ptr @.str.6)
  br label %bb14
bb14:
  ret void
}

define i32 @main() {
entry:
  %t0 = add i32 0, 1
  call void @InputKt_printMonth(i32 %t0)
  %t1 = add i32 0, 3
  call void @InputKt_printMonth(i32 %t1)
  %t2 = add i32 0, 6
  call void @InputKt_printMonth(i32 %t2)
  %t3 = add i32 0, 12
  call void @InputKt_printMonth(i32 %t3)
  ret i32 0
}

